#![no_std]

#[cfg(test)]
mod tests;

/// Slab allocator for my OS
///
/// Well-synergized with buddy allocator
use core::cell::UnsafeCell;
use core::cmp::PartialEq;
use core::ptr::null_mut;
use intrusive_collections::{intrusive_adapter, LinkedList, LinkedListLink, UnsafeRef};
// TODO: It might be worth adding a Drop implementation that will panic if not all objects are freed

/// Slab cache
///
/// Stores objects of the type T
pub struct Cache<T, M: MemoryBackend + Sized> {
    object_size: usize,
    slab_size: usize,
    page_size: usize,
    object_size_type: ObjectSizeType,
    /// Total objects in slab
    objects_per_slab: usize,
    /// List of slabs with free objects (with occupacy less than 75%)
    free_slabs_list_occupacy_less_75: LinkedList<SlabInfoAdapter>,
    /// List of slabs with free objects (with occupacy more than 75%)
    ///
    /// Object taken from this list if available
    free_slabs_list_occupacy_more_75: LinkedList<SlabInfoAdapter>,
    /// Minimum number of allocated objects in more than 75 list
    occupacy_more_75_minimum_allocated_objects_number: usize,
    /// List of full slabs
    full_slabs_list: LinkedList<SlabInfoAdapter>,
    memory_backend: M,
    phantom_data: core::marker::PhantomData<T>,
    statistics: CacheStatistics,
}

impl<T, M: MemoryBackend + Sized> Cache<T, M> {
    /// slab_size must be >= page_size and must be the sum of page_size.<br>
    /// I.e. the start and end of slab must be page-aligned.<br>
    ///
    /// size of T must be >= 8/16 (two pointers)
    ///
    /// Configuration behaviors (Memory Backend requirements):<br>
    /// [ObjectSizeType::Small] && slab_size == page_size: Requires alloc/free slabs.<br>
    /// [ObjectSizeType::Small] && slab_size > page_size: Requires alloc/free slabs and save/get SlabInfo addr.<br>
    /// [ObjectSizeType::Large] && slab_size >= page_size: Requires alloc/free slabs, alloc/release SlabInfo and save/get SlabInfo addr.<br>
    pub fn new(
        slab_size: usize,
        page_size: usize,
        object_size_type: ObjectSizeType,
        memory_backend: M,
    ) -> Result<Self, &'static str> {
        if slab_size % page_size != 0 {
            return Err(
                "slab_size is not exactly within the page boundaries. Slab must consist of pages.",
            );
        }
        if !slab_size.is_power_of_two() {
            return Err("Slab size is not power of two");
        }

        if page_size % align_of::<T>() != 0 {
            return Err("Type can't be aligned");
        }

        let object_size = size_of::<T>();
        if object_size < size_of::<FreeObject>() {
            return Err("Object size smaller than 8/16 (two pointers)");
        };
        if let ObjectSizeType::Small = object_size_type {
            if slab_size < size_of::<SlabInfo>() + object_size {
                return Err("Slab size is too small");
            }
        }
        assert_eq!(size_of::<FreeObject>(), size_of::<*const u8>() * 2);

        // Calculate number of objects in slab
        let objects_per_slab = match object_size_type {
            ObjectSizeType::Small => {
                let fake_slab_addr = 0usize;
                let fake_slab_info_addr = calculate_slab_info_addr_in_small_object_cache(
                    fake_slab_addr as *mut u8,
                    slab_size,
                );
                assert!(fake_slab_info_addr > fake_slab_addr);
                assert!(fake_slab_info_addr <= fake_slab_addr + slab_size - size_of::<SlabInfo>());
                (fake_slab_info_addr - fake_slab_addr) / object_size
            }
            ObjectSizeType::Large => slab_size / object_size,
        };
        if objects_per_slab == 0 {
            return Err("No memory for any object, slab size too small");
        }

        Ok(Self {
            object_size,
            slab_size,
            page_size,
            object_size_type,
            objects_per_slab,
            free_slabs_list_occupacy_less_75: LinkedList::new(SlabInfoAdapter::new()),
            free_slabs_list_occupacy_more_75: LinkedList::new(SlabInfoAdapter::new()),
            occupacy_more_75_minimum_allocated_objects_number: (75 * objects_per_slab) / 100,
            full_slabs_list: LinkedList::new(SlabInfoAdapter::new()),
            memory_backend,
            phantom_data: core::marker::PhantomData,
            statistics: CacheStatistics {
                free_slabs_number: 0,
                full_slabs_number: 0,
                free_objects_number: 0,
                allocated_objects_number: 0,
            },
        })
    }

    /// Allocs object from cache
    ///
    /// # Safety
    /// May return null pointer<br>
    /// Allocated memory is not initialized
    pub unsafe fn alloc(&mut self) -> *mut T {
        if self.free_slabs_list_occupacy_more_75.is_empty()
            && self.free_slabs_list_occupacy_less_75.is_empty()
        {
            // Need to allocate new slab
            let slab_ptr = self
                .memory_backend
                .alloc_slab(self.slab_size, self.page_size);
            if slab_ptr.is_null() {
                return null_mut();
            }

            // Calculate/allocate SlabInfo ptr
            let slab_info_ptr = match self.object_size_type {
                ObjectSizeType::Small => {
                    // SlabInfo stored inside slab, at end
                    let slab_info_addr =
                        calculate_slab_info_addr_in_small_object_cache(slab_ptr, self.slab_size);
                    assert!(slab_info_addr > slab_ptr as usize);
                    assert!(
                        slab_info_addr
                            <= slab_ptr as usize + self.slab_size - size_of::<SlabInfo>()
                    );

                    slab_info_addr as *mut SlabInfo
                }
                ObjectSizeType::Large => {
                    // Allocate memory using memory backend
                    let slab_info_ptr = self.memory_backend.alloc_slab_info();
                    if slab_info_ptr.is_null() {
                        // Failed to allocate SlabInfo
                        // Free slab
                        self.memory_backend
                            .free_slab(slab_ptr, self.slab_size, self.page_size);
                        return null_mut();
                    }
                    assert!(
                        slab_info_ptr.is_aligned(),
                        "Memory backend allocates not aligned SlabInfo"
                    );
                    slab_info_ptr
                }
            };
            assert!(!slab_info_ptr.is_null());
            assert!(slab_info_ptr.is_aligned());

            // Fill SlabInfo
            slab_info_ptr.write(SlabInfo {
                slab_link: LinkedListLink::new(),
                data: UnsafeCell::new(SlabInfoData {
                    free_objects_list: LinkedList::new(FreeObjectAdapter::new()),
                    cache_ptr: self as *mut Self as *mut _,
                    free_objects_number: self.objects_per_slab,
                    slab_ptr,
                }),
            });

            // Make SlabInfo ref
            let slab_info_ref = UnsafeRef::from_raw(slab_info_ptr);
            // Add SlabInfo to free list
            self.free_slabs_list_occupacy_less_75
                .push_back(slab_info_ref);
            self.statistics.free_slabs_number += 1;
            self.statistics.free_objects_number += self.objects_per_slab;

            // Fill FreeObjects list
            for free_object_index in 0..self.objects_per_slab {
                // Free object stored in slab
                let free_object_addr = slab_ptr as usize + (free_object_index * self.object_size);
                assert_eq!(
                    free_object_addr % align_of::<FreeObject>(),
                    0,
                    "FreeObject addr not aligned!"
                );
                let free_object_ptr = free_object_addr as *mut FreeObject;
                free_object_ptr.write(FreeObject {
                    free_object_link: LinkedListLink::new(),
                });
                let free_object_ref = UnsafeRef::from_raw(free_object_ptr);

                // Add free object to free objects list
                (*self
                    .free_slabs_list_occupacy_less_75
                    .front()
                    .get()
                    .unwrap()
                    .data
                    .get())
                .free_objects_list
                .push_back(free_object_ref);
            }
        }
        // Allocate object

        // Get free slab info
        let free_slab_info = {
            // First we try to choose the slab with the highest occupancy.
            // This should allow to concentrate the allocations inside the most occupied slabs,
            // while slabs with a small allocated number of objects are more likely to be freed.
            if let Some(slab_info) = self.free_slabs_list_occupacy_more_75.front().get() {
                slab_info
            } else {
                self.free_slabs_list_occupacy_less_75.front().get().unwrap()
            }
        };
        // Get slab data
        let free_slab_info_data = &mut *free_slab_info.data.get();

        // Get object from FreeObject list
        let free_object_ref = free_slab_info_data.free_objects_list.pop_back().unwrap();
        free_slab_info_data.free_objects_number -= 1;
        self.statistics.free_objects_number -= 1;
        let free_object_ptr = UnsafeRef::<FreeObject>::into_raw(free_object_ref);

        // Save SlabInfo ptr
        if !(self.object_size_type == ObjectSizeType::Small && self.slab_size == self.page_size) {
            let free_slab_info_ptr = free_slab_info as *const _ as *mut _;
            let free_object_page_addr = align_down(free_object_ptr as usize, self.page_size);
            debug_assert_eq!(free_object_page_addr % self.page_size, 0);

            // In this case we can avoid unnecessary saving for this page, if it already has allocated objects, the slab into ptr is already saved.
            let mut dont_save = false;
            if self.objects_per_slab >= 2 {
                dont_save = self.slab_size == self.page_size
                    && free_slab_info_data.free_objects_number <= self.objects_per_slab - 2;
            }

            if !dont_save {
                self.memory_backend
                    .save_slab_info_ptr(free_object_page_addr, free_slab_info_ptr);
            }
        }

        // Slab occupacy become more than 75? (free (<75) -> free (>75))
        let allocated_objects_number =
            self.objects_per_slab - (*free_slab_info.data.get()).free_objects_number;
        let previously_was_in_less_75_list =
            allocated_objects_number - 1 < self.occupacy_more_75_minimum_allocated_objects_number;
        let now_in_more_75_list =
            allocated_objects_number >= self.occupacy_more_75_minimum_allocated_objects_number;
        if previously_was_in_less_75_list && now_in_more_75_list {
            // Move slab info from free (<75) to free (>75)
            let mut slab_info_free_less_75_list_cursor = self
                .free_slabs_list_occupacy_less_75
                .cursor_mut_from_ptr(free_slab_info as *const SlabInfo);
            let free_slab_info = slab_info_free_less_75_list_cursor.remove().unwrap();

            // Add to free (>75)
            self.free_slabs_list_occupacy_more_75
                .push_front(free_slab_info);
        }

        // Slab become empty? (free (>75) -> full)
        if free_slab_info_data.free_objects_list.is_empty() {
            // Slab is empty now
            // Remove from free list
            let free_slab_info = self.free_slabs_list_occupacy_more_75.pop_front().unwrap();
            self.statistics.free_slabs_number -= 1;
            // Add to full list
            self.full_slabs_list.push_back(free_slab_info);
            self.statistics.full_slabs_number += 1;
        }

        self.statistics.allocated_objects_number += 1;
        free_object_ptr.cast()
    }

    /// Returns object to cache
    ///
    /// # Safety
    /// Pointer must be a previously allocated pointer from the same cache
    pub unsafe fn free(&mut self, object_ptr: *mut T) {
        assert!(!object_ptr.is_null(), "Try to free null ptr");
        assert!(
            object_ptr.is_aligned(),
            "Try to free null ptr (aligned pointer has been allocated)"
        );

        // Calculate/Get slab_addr and slab_info_addr
        let (slab_addr, slab_info_addr) = {
            if self.object_size_type == ObjectSizeType::Small && self.slab_size == self.page_size {
                // In this case we may calculate slab info addr
                let slab_addr = align_down(object_ptr as usize, self.page_size);
                let slab_info_addr = calculate_slab_info_addr_in_small_object_cache(
                    slab_addr as *mut u8,
                    self.slab_size,
                );
                assert_ne!(slab_addr, 0);
                assert_ne!(slab_info_addr, 0);
                debug_assert!(slab_info_addr > slab_addr);
                debug_assert!(slab_info_addr <= slab_addr + self.slab_size - size_of::<SlabInfo>());
                assert_eq!(slab_info_addr % align_of::<SlabInfo>(), 0);
                (slab_addr, slab_info_addr)
            } else {
                // Get slab info addr from memory backend
                let object_addr = object_ptr as usize;
                let object_page_addr = align_down(object_addr, self.page_size);
                let slab_info_ptr = self.memory_backend.get_slab_info_ptr(object_page_addr);
                assert!(!slab_info_ptr.is_null());
                assert!(slab_info_ptr.is_aligned());
                let slab_ptr = (*(*slab_info_ptr).data.get()).slab_ptr;
                assert!(!slab_ptr.is_null());
                (slab_ptr as usize, slab_info_ptr as usize)
            }
        };
        let free_object_ptr = object_ptr as *mut FreeObject;
        free_object_ptr.write(FreeObject {
            free_object_link: LinkedListLink::new(),
        });

        // Return object to slab
        let free_object_ref = UnsafeRef::from_raw(free_object_ptr);
        let slab_info_ptr = slab_info_addr as *mut SlabInfo;
        let slab_info_ref = UnsafeRef::from_raw(slab_info_ptr);

        // Check cache
        assert_eq!((*slab_info_ref.data.get()).cache_ptr, self as *mut _ as *mut u8, "It was not possible to verify that the object belongs to the cache. It looks like you try free an invalid address.");
        assert_ne!((*slab_info_ref.data.get()).free_objects_number, self.objects_per_slab, "Attempting to free an unallocated object! There are no allocated objects in this slab. It looks like invalid address or double free.");

        // Add object to free list
        (*slab_info_ref.data.get())
            .free_objects_list
            .push_back(free_object_ref);
        (*slab_info_ref.data.get()).free_objects_number += 1;
        self.statistics.free_objects_number += 1;
        self.statistics.allocated_objects_number -= 1;

        // Slab become free? (full -> free (>75))
        if (*slab_info_ref.data.get()).free_objects_number == 1 {
            // Move slab info from full list to free
            let mut slab_info_full_list_cursor =
                self.full_slabs_list.cursor_mut_from_ptr(slab_info_ptr);
            self.statistics.full_slabs_number -= 1;
            assert!(slab_info_full_list_cursor.remove().is_some());

            // Add slab to free list
            self.free_slabs_list_occupacy_more_75
                .push_front(slab_info_ref.clone());
            self.statistics.free_slabs_number += 1;
        }

        // Slab occupacy become less than 75? (free (>75) -> free (<75))
        let allocated_objects_number =
            self.objects_per_slab - (*slab_info_ref.data.get()).free_objects_number;
        let previously_was_in_more_75_list =
            allocated_objects_number + 1 >= self.occupacy_more_75_minimum_allocated_objects_number;
        let now_in_less_75_list =
            allocated_objects_number < self.occupacy_more_75_minimum_allocated_objects_number;
        if previously_was_in_more_75_list && now_in_less_75_list {
            // Move slab info from free (>75) to free (<75)
            let mut slab_info_free_more_75_list_cursor = self
                .free_slabs_list_occupacy_more_75
                .cursor_mut_from_ptr(slab_info_ptr);
            assert!(slab_info_free_more_75_list_cursor.remove().is_some());

            // Add to free (<75)
            self.free_slabs_list_occupacy_less_75
                .push_front(UnsafeRef::from_raw(slab_info_ptr));
        }

        // List becomes empty?
        if (*slab_info_ptr).data.get_mut().free_objects_number == self.objects_per_slab {
            // All objects in slab is free - free slab
            // Remove SlabInfo from free list
            let mut slab_info_free_list_cursor = self
                .free_slabs_list_occupacy_less_75
                .cursor_mut_from_ptr(slab_info_ptr);
            assert!(slab_info_free_list_cursor.remove().is_some());
            self.statistics.free_slabs_number -= 1;
            self.statistics.free_objects_number -= self.objects_per_slab;

            // Free slab memory
            self.memory_backend
                .free_slab(slab_addr as *mut u8, self.slab_size, self.page_size);

            if !(self.object_size_type == ObjectSizeType::Small && self.slab_size == self.page_size)
            {
                if self.object_size_type == ObjectSizeType::Large {
                    // Free SlabInfo
                    self.memory_backend.free_slab_info(slab_info_ptr);
                }
                for i in 0..(self.slab_size / self.page_size) {
                    let page_addr = slab_addr + (i * self.page_size);
                    self.memory_backend.delete_slab_info_ptr(page_addr);
                }
            }
        }
    }

    /// Gets object size in bytes
    pub fn object_size(&self) -> usize {
        self.object_size
    }

    /// Gets slab size in bytes
    pub fn slab_size(&self) -> usize {
        self.slab_size
    }

    /// Gets page size in bytes
    pub fn page_size(&self) -> usize {
        self.page_size
    }

    /// Gets ObjectSizeType
    pub fn object_size_type(&self) -> ObjectSizeType {
        self.object_size_type
    }

    /// Gets objects per slab in bytes
    pub fn objects_per_slab(&self) -> usize {
        self.objects_per_slab
    }

    /// Gets cache statistics
    pub fn cache_statistics(&self) -> CacheStatistics {
        self.statistics
    }
}

fn calculate_slab_info_addr_in_small_object_cache(slab_ptr: *mut u8, slab_size: usize) -> usize {
    // SlabInfo inside slab, at end
    let slab_info_addr = (slab_ptr as usize + slab_size) - size_of::<SlabInfo>();
    align_down(slab_info_addr, align_of::<SlabInfo>())
}

fn align_down(addr: usize, align: usize) -> usize {
    addr & !(align - 1)
}

/// See README.md, [ObjectSizeType::Small] and [ObjectSizeType::Large]
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum ObjectSizeType {
    /// For small size objects, SlabInfo is stored directly in slab and little memory is lost.<br>
    /// For example:<br>
    /// slab size: 4096<br>
    /// object size: 32<br>
    /// slab info: 40<br>
    /// We will be able to place 126 objects, this will consume 4032 bytes, the 40 bytes will be occupied by SlabInfo, only 24 bytes will be lost, all is well.
    Small,
    /// For large size objects, SlabInfo can't be stored directly in slab and allocates using MemoryBackend.<br>
    /// For example:<br>
    /// slab size: 4096<br>
    /// object size: 2048<br>
    /// slab info: 40<br>
    /// We will be able to place only 1 objects, this will consume 2048 bytes, the 40 bytes will be occupied by SlabInfo, 2008 bytes will be lost!
    Large,
}

/// Slab info
///
/// Stored in slab(for small objects slab) or allocatated from another slab(for large objects slab)
#[repr(C)]
pub struct SlabInfo {
    /// Link to next and prev slab
    slab_link: LinkedListLink,
    /// LinkedList doesn't give mutable access to data, we have to snip the data in UnsafeCell
    data: UnsafeCell<SlabInfoData>,
}

// To use Cache in static, the compiler requires the implementation of Sync and Send for SlabInfo.
// But this is not required because it is an internal structure and is not used outside the Cache code,
// and Cache access itself will always be synchronised externally.
// Even if I add synchronisation primitives here, it won't help.
unsafe impl Send for SlabInfo {}
unsafe impl Sync for SlabInfo {}

struct SlabInfoData {
    /// Free objects in slab list
    free_objects_list: LinkedList<FreeObjectAdapter>,
    /// Slab cache to which slab belongs
    cache_ptr: *mut u8,
    /// Number of free objects in slab
    free_objects_number: usize,
    /// Slab ptr
    slab_ptr: *mut u8,
}

#[derive(Debug)]
#[repr(transparent)]
/// Metadata stored inside a free object and pointing to the previous and next free object
struct FreeObject {
    free_object_link: LinkedListLink,
}

intrusive_adapter!(SlabInfoAdapter = UnsafeRef<SlabInfo>: SlabInfo { slab_link: LinkedListLink });
intrusive_adapter!(FreeObjectAdapter = UnsafeRef<FreeObject>: FreeObject { free_object_link: LinkedListLink });

/// Used by slab cache for allocating slabs, SlabInfo's, saving/geting SlabInfo addrs
///
/// Slab caching logic can be placed here
///
/// See [Cache::new()] for memory backend requirements
pub trait MemoryBackend {
    /// Allocates slab for cache
    ///
    /// # Safety
    /// Must be page aligned
    unsafe fn alloc_slab(&mut self, slab_size: usize, page_size: usize) -> *mut u8;

    /// Frees slab
    unsafe fn free_slab(&mut self, slab_ptr: *mut u8, slab_size: usize, page_size: usize);

    /// Allocs SlabInfo
    unsafe fn alloc_slab_info(&mut self) -> *mut SlabInfo;

    /// Frees SlabInfo
    unsafe fn free_slab_info(&mut self, slab_info_ptr: *mut SlabInfo);

    /// It is required to save slab_info_ptr to the corresponding object page addr
    ///
    /// This function cannot be called just for the cache which: [ObjectSizeType::Small] and slab_size == page_size.<br>
    /// In this case the allocator is able to calculate its address itself.
    ///
    /// How it works: when an object is returned to the cache, SlabInfo needs to be found.<br>
    /// SlabInfo can be stored in the slab itself([ObjectSizeType::Small]) or outside of it([ObjectSizeType::Large]).<br>
    /// In the case of Large it is impossible to calculate this address exactly, in the case of Small it is impossible to calculate this address because the start of the slab is unknown.<br>
    /// Only when slab_size == page_size, we know for sure that the slab starts at the beginning of the page.
    ///
    ///
    /// # IMPORTANT
    /// * Since the beginning of slab is aligned to the beginning of a page, and slab exactly in pages you can save slab_info_ptr to the page address of the page to which the object belongs.<br>
    ///   Hash table good for this
    /// ```ignore
    /// // key value
    /// saved_slab_infos_ht.insert(object_page_addr, slab_info_ptr);
    /// ```
    ///
    ///  |   SLAB0   | <-- 1 slabs, 1 slab info<br>
    ///  |o0;o1|o2;o3| <-- 2 pages (2 pages in slab)<br>
    /// If you align the address of the object to the page, you can unambiguously refer it to the correct slab (slab page) and calculate SlabInfo by the slab page as well.<br>
    /// Not only is it incredibly wasteful to save SlabInfo for each object, but it doesn't make sense. But this trick works only when the beginning of the slab is aligned to the beginning of the page and when its size is the sum of page sizes.
    unsafe fn save_slab_info_ptr(&mut self, object_page_addr: usize, slab_info_ptr: *mut SlabInfo);

    /// It is required to get slab_info_ptr to the corresponding object page addr
    unsafe fn get_slab_info_ptr(&mut self, object_page_addr: usize) -> *mut SlabInfo;

    /// Notify that the SlabInfo for the page can be deleted(if exist)
    ///
    /// Called when slab is freed by the allocator
    ///
    /// # ATTENTION!
    ///
    /// This method is called for every page in the slab, even SlabInfo was not stored for that page.<br>
    /// If it is dangerous to delete a non-existing element in your code, you should always check if it really exists.
    /// ```ignored
    /// if saved_slab_infos_ht.key_exist(page_addr) {
    ///     saved_slab_infos_ht.remove(page_addr);
    /// }
    /// ```
    unsafe fn delete_slab_info_ptr(&mut self, page_addr: usize);
}

#[derive(Debug, Clone, Copy)]
pub struct CacheStatistics {
    /// Number of slabs with free objects
    pub free_slabs_number: usize,
    /// Number of slabs in which all objects are allocated
    pub full_slabs_number: usize,
    /// Number of objects in cache available for allocation without Slab allocation
    pub free_objects_number: usize,
    /// Number of objects in cache allocated from Cache
    pub allocated_objects_number: usize,
}
