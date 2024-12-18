#![no_std]

/// Slab cache for my OS
///
/// Well-synergized with buddy allocator
use core::cell::UnsafeCell;
use core::cmp::PartialEq;
use core::ptr::null_mut;
use intrusive_collections::{
    intrusive_adapter, LinkedList, LinkedListAtomicLink, LinkedListLink, UnsafeRef,
};
use spin::Mutex;
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
    /// List of slabs with free objects
    free_slabs_list: LinkedList<SlabInfoAdapter>,
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
        let object_size = size_of::<T>();
        if object_size < size_of::<FreeObject>() {
            return Err("Object size smaller than 8/16 (two pointers)");
        };
        if let ObjectSizeType::Small = object_size_type {
            if slab_size < size_of::<SlabInfo>() + object_size {
                return Err("Slab size is too small");
            }
        }
        if slab_size % page_size != 0 {
            return Err(
                "slab_size is not exactly within the page boundaries. Slab must consist of pages.",
            );
        }
        if page_size % align_of::<T>() != 0 {
            return Err("Type can't be aligned");
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
            free_slabs_list: LinkedList::new(SlabInfoAdapter::new()),
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
        if self.free_slabs_list.is_empty() {
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
                slab_link: LinkedListAtomicLink::new(),
                data: Mutex::new(UnsafeCell::new(SlabInfoData {
                    free_objects_list: LinkedList::new(FreeObjectAdapter::new()),
                    cache_ptr: self as *mut Self as *mut _,
                    free_objects_number: self.objects_per_slab,
                    slab_ptr,
                })),
            });

            // Make SlabInfo ref
            let slab_info_ref = UnsafeRef::from_raw(slab_info_ptr);
            // Add SlabInfo to free list
            self.free_slabs_list.push_back(slab_info_ref);
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
                self.free_slabs_list
                    .front()
                    .get()
                    .unwrap()
                    .data
                    .lock()
                    .get_mut()
                    .free_objects_list
                    .push_back(free_object_ref);
            }
        }
        // Allocate object

        // Get free slab info
        let free_slab_info = self.free_slabs_list.front().get().unwrap();
        // Get slab data
        let free_slab_info_data = &mut *free_slab_info.data.lock().get();

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
                    .save_slab_info_addr(free_object_page_addr, free_slab_info_ptr);
            }
        }

        // Slab became empty?
        if free_slab_info_data.free_objects_list.is_empty() {
            // Slab is empty now
            // Remove from free list
            let free_slab_info = self.free_slabs_list.pop_front().unwrap();
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
                let slab_info_ptr = self.memory_backend.get_slab_info_addr(object_page_addr);
                assert!(!slab_info_ptr.is_null());
                assert!(slab_info_ptr.is_aligned());
                let slab_ptr = (*(*slab_info_ptr).data.lock().get()).slab_ptr;
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
        assert_eq!((*slab_info_ref.data.lock().get()).cache_ptr, self as *mut _ as *mut u8, "It was not possible to verify that the object belongs to the cache. It looks like you try free an invalid address.");
        assert_ne!((*slab_info_ref.data.lock().get()).free_objects_number, self.objects_per_slab, "Attempting to free an unallocated object! There are no allocated objects in this slab. It looks like invalid address or double free.");

        // Add object to free list
        (*slab_info_ref.data.lock().get())
            .free_objects_list
            .push_back(free_object_ref);
        (*slab_info_ref.data.lock().get()).free_objects_number += 1;
        self.statistics.free_objects_number += 1;
        self.statistics.allocated_objects_number -= 1;

        // Slab became free? (full -> free)
        if slab_info_ref.data.lock().get_mut().free_objects_number == 1 {
            // Move slab info from full list to free
            let mut slab_info_full_list_cursor =
                self.full_slabs_list.cursor_mut_from_ptr(slab_info_ptr);
            self.statistics.full_slabs_number -= 1;
            assert!(slab_info_full_list_cursor.remove().is_some());

            // Add slab to start
            // It is more likely to be used again because when we alloc object, we take slab from the front
            self.free_slabs_list.push_front(slab_info_ref);
            self.statistics.free_slabs_number += 1;
        }

        // List becomes empty?
        if (*slab_info_ptr).data.lock().get_mut().free_objects_number == self.objects_per_slab {
            // All objects in slab is free - free slab
            // Remove SlabInfo from free list
            let mut slab_info_free_list_cursor =
                self.free_slabs_list.cursor_mut_from_ptr(slab_info_ptr);
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
                    self.memory_backend.delete_slab_info_addr(page_addr);
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

/// See [ObjectSizeType::Small] and [ObjectSizeType::Large]
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
    slab_link: LinkedListAtomicLink,
    /// LinkedList doesn't give mutable access to data, we have to snip the data in UnsafeCell
    data: Mutex<UnsafeCell<SlabInfoData>>,
}

// LinkedListAtomic and Mutex are Send + Sync
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

    /// It is required to save slab_info_addr to the corresponding ***down page aligned*** object_ptr (page addr)
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
    unsafe fn save_slab_info_addr(&mut self, object_page_addr: usize, slab_info_ptr: *mut SlabInfo);

    /// It is required to get slab_info_addr the corresponding ***down page aligned*** object_ptr (page addr)
    unsafe fn get_slab_info_addr(&mut self, object_page_addr: usize) -> *mut SlabInfo;

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
    unsafe fn delete_slab_info_addr(&mut self, page_addr: usize);
}

#[derive(Debug, Clone, Copy)]
pub struct CacheStatistics {
    /// Number of slabs with free objects
    pub free_slabs_number: usize,
    /// Number of slabs in which all objects are allocated
    pub full_slabs_number: usize,
    /// Number of objects in cache available for allocation without Slab allocation
    pub free_objects_number: usize,
    /// Number of objects in cache allocated from slab
    pub allocated_objects_number: usize,
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;
    extern crate alloc;
    extern crate std;
    use alloc::alloc::{alloc, dealloc, Layout};
    use alloc::vec;
    use alloc::vec::Vec;
    use rand::prelude::SliceRandom;
    use rand::Rng;
    use spin::{Mutex, Once};
    use std::collections::{HashMap, HashSet};

    #[test]
    fn can_be_used_as_static() {
        let test_memory_backend: TestMemoryBackend = TestMemoryBackend;

        static CACHE: Mutex<Once<Cache<i128, TestMemoryBackend>>> = Mutex::new(Once::new());

        CACHE.lock().call_once(|| {
            Cache::new(4096, 4096, ObjectSizeType::Small, test_memory_backend).unwrap()
        });

        struct TestMemoryBackend;

        impl MemoryBackend for TestMemoryBackend {
            unsafe fn alloc_slab(&mut self, _slab_size: usize, _page_size: usize) -> *mut u8 {
                unreachable!();
            }

            unsafe fn free_slab(
                &mut self,
                _slab_ptr: *mut u8,
                _slab_size: usize,
                _page_size: usize,
            ) {
                unreachable!();
            }

            unsafe fn alloc_slab_info(&mut self) -> *mut SlabInfo {
                unreachable!();
            }

            unsafe fn free_slab_info(&mut self, _slab_info_ptr: *mut SlabInfo) {
                unreachable!();
            }

            unsafe fn save_slab_info_addr(
                &mut self,
                _object_page_addr: usize,
                _slab_info_ptr: *mut SlabInfo,
            ) {
                unreachable!();
            }

            unsafe fn get_slab_info_addr(&mut self, _object_page_addr: usize) -> *mut SlabInfo {
                unreachable!();
            }

            unsafe fn delete_slab_info_addr(&mut self, _page_addr: usize) {
                unreachable!();
            }
        }
    }

    // Allocations only
    // Small, slab size == page size
    // No SlabInfo allocation
    // No SlabInfo save/get
    #[test]
    fn _00_alloc_only_small_ss_eq_ps() {
        unsafe {
            const PAGE_SIZE: usize = 4096;
            const SLAB_SIZE: usize = 4096;
            const OBJECT_SIZE_TYPE: ObjectSizeType = ObjectSizeType::Small;

            struct TestObjectType1024 {
                #[allow(unused)]
                a: [u64; 1024 / 8],
            }
            assert_eq!(size_of::<TestObjectType1024>(), 1024);

            struct TestMemoryBackend {
                allocated_slab_addrs: Vec<usize>,
            }

            impl MemoryBackend for TestMemoryBackend {
                unsafe fn alloc_slab(&mut self, slab_size: usize, page_size: usize) -> *mut u8 {
                    assert_eq!(slab_size, SLAB_SIZE);
                    assert_eq!(page_size, PAGE_SIZE);
                    let layout = Layout::from_size_align(slab_size, page_size).unwrap();
                    let allocated_slab_ptr = alloc(layout);
                    assert!(!allocated_slab_ptr.is_null());
                    self.allocated_slab_addrs.push(allocated_slab_ptr as usize);
                    allocated_slab_ptr
                }

                unsafe fn free_slab(
                    &mut self,
                    _slab_ptr: *mut u8,
                    _slab_size: usize,
                    _page_size: usize,
                ) {
                    unreachable!();
                }

                unsafe fn alloc_slab_info(&mut self) -> *mut SlabInfo {
                    unreachable!();
                }

                unsafe fn free_slab_info(&mut self, _slab_info_ptr: *mut SlabInfo) {
                    unreachable!();
                }

                unsafe fn save_slab_info_addr(
                    &mut self,
                    _object_page_addr: usize,
                    _slab_info_ptr: *mut SlabInfo,
                ) {
                    unreachable!();
                }

                unsafe fn get_slab_info_addr(&mut self, _object_page_addr: usize) -> *mut SlabInfo {
                    unreachable!();
                }

                unsafe fn delete_slab_info_addr(&mut self, _page_addr: usize) {
                    unreachable!();
                }
            }

            let test_memory_backend = TestMemoryBackend {
                allocated_slab_addrs: Vec::new(),
            };

            // Create cache
            // 3 objects
            // [obj0, obj1, obj2]
            let mut cache: Cache<TestObjectType1024, TestMemoryBackend> =
                Cache::new(SLAB_SIZE, PAGE_SIZE, OBJECT_SIZE_TYPE, test_memory_backend).unwrap();
            assert_eq!(cache.objects_per_slab, 3);

            // Alloc 7 objects
            let mut allocated_ptrs = [null_mut(); 7];
            for v in allocated_ptrs.iter_mut() {
                *v = cache.alloc();
                assert!(!v.is_null());
                assert!(v.is_aligned());
            }
            // slab 0            slab 1            slab2
            // [obj2, obj1, obj0][obj2, obj1, obj0][obj2]
            let mut obj_index_in_slab = cache.objects_per_slab - 1;
            for (i, v) in allocated_ptrs.iter().enumerate() {
                // 0 0 0 1 1 1 2
                let slab_index = i / cache.objects_per_slab;
                // 0 1 2 3 4 5 6
                // 2 1 0 2 1 0 2
                let object_addr = cache.memory_backend.allocated_slab_addrs[slab_index]
                    + obj_index_in_slab * cache.object_size;
                if obj_index_in_slab == 0 {
                    obj_index_in_slab = cache.objects_per_slab - 1;
                } else {
                    obj_index_in_slab -= 1;
                }
                assert_eq!(*v as usize, object_addr);
            }

            // 1 free, 2 full slabs
            assert_eq!(cache.free_slabs_list.iter().count(), 1);
            assert_eq!(cache.full_slabs_list.iter().count(), 2);
            // 2 free objects
            assert_eq!(
                (*cache
                    .free_slabs_list
                    .back()
                    .get()
                    .unwrap()
                    .data
                    .lock()
                    .get())
                .free_objects_list
                .iter()
                .count(),
                2
            );
            assert_eq!(
                (*cache
                    .free_slabs_list
                    .back()
                    .get()
                    .unwrap()
                    .data
                    .lock()
                    .get())
                .free_objects_number,
                2
            );

            // Alloc 2
            assert!(!cache.alloc().is_null());
            assert!(!cache.alloc().is_null());
            // 0 free, 3 full slabs
            assert_eq!(cache.free_slabs_list.iter().count(), 0);
            assert_eq!(cache.full_slabs_list.iter().count(), 3);

            // Check statistics
            assert_eq!(cache.statistics.free_slabs_number, 0);
            assert_eq!(cache.statistics.full_slabs_number, 3);
            assert_eq!(cache.statistics.allocated_objects_number, 9);
            assert_eq!(cache.statistics.free_objects_number, 0);

            // Free slabs manualy (alloc test only)
            let allocated_slab_addrs = cache.memory_backend.allocated_slab_addrs.clone();

            drop(cache);

            for addr in allocated_slab_addrs {
                let layout = Layout::from_size_align(SLAB_SIZE, PAGE_SIZE).unwrap();
                dealloc(addr as *mut u8, layout);
            }
        }
    }

    // Allocations only
    // Small, slab size > page size
    // No SlabInfo allocation
    // SlabInfo save
    #[test]
    fn _01_alloc_only_small_ss_neq_ps() {
        unsafe {
            const PAGE_SIZE: usize = 4096;
            const SLAB_SIZE: usize = 8192;
            const OBJECT_SIZE_TYPE: ObjectSizeType = ObjectSizeType::Small;

            struct TestObjectType1024 {
                #[allow(unused)]
                a: [u64; 1024 / 8],
            }
            assert_eq!(size_of::<TestObjectType1024>(), 1024);

            struct TestMemoryBackend {
                allocated_slab_addrs: Vec<usize>,
            }

            impl MemoryBackend for TestMemoryBackend {
                unsafe fn alloc_slab(&mut self, slab_size: usize, page_size: usize) -> *mut u8 {
                    assert_eq!(slab_size, SLAB_SIZE);
                    assert_eq!(page_size, PAGE_SIZE);
                    let layout = Layout::from_size_align(slab_size, page_size).unwrap();
                    let allocated_slab_ptr = alloc(layout);
                    self.allocated_slab_addrs.push(allocated_slab_ptr as usize);
                    allocated_slab_ptr
                }

                unsafe fn free_slab(
                    &mut self,
                    _slab_ptr: *mut u8,
                    _slab_size: usize,
                    _page_size: usize,
                ) {
                    unreachable!();
                }

                unsafe fn alloc_slab_info(&mut self) -> *mut SlabInfo {
                    unreachable!();
                }

                unsafe fn free_slab_info(&mut self, _slab_info_ptr: *mut SlabInfo) {
                    unreachable!();
                }

                unsafe fn save_slab_info_addr(
                    &mut self,
                    object_page_addr: usize,
                    slab_info_ptr: *mut SlabInfo,
                ) {
                    assert!(!slab_info_ptr.is_null());
                    assert!(slab_info_ptr.is_aligned());
                    assert_eq!(object_page_addr % PAGE_SIZE, 0);
                    // Get function not call's in this test
                }

                unsafe fn get_slab_info_addr(&mut self, _object_page_addr: usize) -> *mut SlabInfo {
                    unreachable!();
                }

                unsafe fn delete_slab_info_addr(&mut self, _page_addr: usize) {}
            }

            let test_memory_backend = TestMemoryBackend {
                allocated_slab_addrs: Vec::new(),
            };

            // Create cache
            // 7 objects
            // [obj0, obj1, obj2, obj3, obj4, obj5, obj6]
            let mut cache: Cache<TestObjectType1024, TestMemoryBackend> =
                Cache::new(SLAB_SIZE, PAGE_SIZE, OBJECT_SIZE_TYPE, test_memory_backend).unwrap();
            assert_eq!(cache.objects_per_slab, 7);

            // Alloc 25 objects
            let mut allocated_ptrs = [null_mut(); 25];
            for v in allocated_ptrs.iter_mut() {
                *v = cache.alloc();
                assert!(!v.is_null());
                assert!(v.is_aligned());
            }
            // slab0
            // [obj6, obj5, obj4, obj3, obj2, obj1, obj0]
            // slab1
            // [obj6, obj5, obj4, obj3, obj2, obj1, obj0]
            // slab2
            // [obj6, obj5, obj4, obj3, obj2, obj1, obj0]
            // slab3
            // [obj6, obj5, obj4, obj3]
            let mut obj_index_in_slab = cache.objects_per_slab - 1;
            for (i, v) in allocated_ptrs.iter().enumerate() {
                // 0 0 0 0 0 0 0
                // 1 1 1 1 1 1 1
                // 2 2 2 2 2 2 2
                // 3 3 3 3
                let slab_index = i / cache.objects_per_slab;
                // 0 1 2 3 4 5 6
                // 2 1 0 2 1 0 2
                let object_addr = cache.memory_backend.allocated_slab_addrs[slab_index]
                    + obj_index_in_slab * cache.object_size;
                if obj_index_in_slab == 0 {
                    obj_index_in_slab = cache.objects_per_slab - 1;
                } else {
                    obj_index_in_slab -= 1;
                }
                assert_eq!(*v as usize, object_addr);
            }

            // 1 free, 3 full slabs
            assert_eq!(cache.free_slabs_list.iter().count(), 1);
            assert_eq!(cache.full_slabs_list.iter().count(), 3);
            // 3 free objects
            assert_eq!(
                (*cache
                    .free_slabs_list
                    .back()
                    .get()
                    .unwrap()
                    .data
                    .lock()
                    .get())
                .free_objects_list
                .iter()
                .count(),
                3
            );
            assert_eq!(
                (*cache
                    .free_slabs_list
                    .back()
                    .get()
                    .unwrap()
                    .data
                    .lock()
                    .get())
                .free_objects_number,
                3
            );

            // Check statistics
            assert_eq!(cache.statistics.free_slabs_number, 1);
            assert_eq!(cache.statistics.full_slabs_number, 3);
            assert_eq!(cache.statistics.allocated_objects_number, 25);
            assert_eq!(cache.statistics.free_objects_number, 3);

            // Free slabs manualy (alloc test only)
            let allocated_slab_addrs = cache.memory_backend.allocated_slab_addrs.clone();

            drop(cache);

            for addr in allocated_slab_addrs {
                let layout = Layout::from_size_align(SLAB_SIZE, PAGE_SIZE).unwrap();
                dealloc(addr as *mut u8, layout);
            }
        }
    }

    // Allocations only
    // Large, slab size == page size
    // SlabInfo allocation
    // SlabInfo save
    #[test]
    fn _02_alloc_only_large_ss_eq_ps() {
        unsafe {
            const PAGE_SIZE: usize = 4096;
            const SLAB_SIZE: usize = 4096;
            const OBJECT_SIZE_TYPE: ObjectSizeType = ObjectSizeType::Large;

            struct TestObjectType56 {
                #[allow(unused)]
                a: [u64; 56 / 8],
            }
            assert_eq!(size_of::<TestObjectType56>(), 56);

            struct TestMemoryBackend {
                allocated_slab_addrs: Vec<usize>,
                allocated_slab_infos_addrs: Vec<usize>,
            }

            impl MemoryBackend for TestMemoryBackend {
                unsafe fn alloc_slab(&mut self, slab_size: usize, page_size: usize) -> *mut u8 {
                    assert_eq!(slab_size, SLAB_SIZE);
                    assert_eq!(page_size, PAGE_SIZE);
                    let layout = Layout::from_size_align(slab_size, page_size).unwrap();
                    let allocated_slab_ptr = alloc(layout);
                    self.allocated_slab_addrs.push(allocated_slab_ptr as usize);
                    allocated_slab_ptr
                }

                unsafe fn free_slab(
                    &mut self,
                    _slab_ptr: *mut u8,
                    _slab_size: usize,
                    _page_size: usize,
                ) {
                    unreachable!();
                }

                unsafe fn alloc_slab_info(&mut self) -> *mut SlabInfo {
                    let layout =
                        Layout::from_size_align(size_of::<SlabInfo>(), align_of::<SlabInfo>())
                            .unwrap();
                    let allocated_slab_info_ptr = alloc(layout);
                    self.allocated_slab_infos_addrs
                        .push(allocated_slab_info_ptr as usize);
                    allocated_slab_info_ptr.cast()
                }

                unsafe fn free_slab_info(&mut self, _slab_info_ptr: *mut SlabInfo) {
                    unreachable!();
                }

                unsafe fn save_slab_info_addr(
                    &mut self,
                    object_page_addr: usize,
                    slab_info_ptr: *mut SlabInfo,
                ) {
                    assert!(!slab_info_ptr.is_null());
                    assert!(slab_info_ptr.is_aligned());
                    assert_eq!(object_page_addr % PAGE_SIZE, 0);
                    // Get function not call's in this test
                }

                unsafe fn get_slab_info_addr(&mut self, _object_page_addr: usize) -> *mut SlabInfo {
                    unreachable!();
                }

                unsafe fn delete_slab_info_addr(&mut self, _page_addr: usize) {}
            }

            let test_memory_backend = TestMemoryBackend {
                allocated_slab_addrs: Vec::new(),
                allocated_slab_infos_addrs: Vec::new(),
            };

            // Create cache
            // 73 objects
            // [obj0, ..., obj72]
            let mut cache: Cache<TestObjectType56, TestMemoryBackend> =
                Cache::new(SLAB_SIZE, PAGE_SIZE, OBJECT_SIZE_TYPE, test_memory_backend).unwrap();
            assert_eq!(cache.objects_per_slab, 73);

            // Alloc 100 objects
            let mut allocated_ptrs = [null_mut(); 100];
            for v in allocated_ptrs.iter_mut() {
                *v = cache.alloc();
                assert!(!v.is_null());
                assert!(v.is_aligned());
            }
            // slab0
            // [obj72, ..., obj0] 73
            // slab1
            // [obj26, ..., obj0] 27
            let mut obj_index_in_slab = cache.objects_per_slab - 1;
            for (i, v) in allocated_ptrs.iter().enumerate() {
                let slab_index = i / cache.objects_per_slab;
                let object_addr = cache.memory_backend.allocated_slab_addrs[slab_index]
                    + obj_index_in_slab * cache.object_size;
                if obj_index_in_slab == 0 {
                    obj_index_in_slab = cache.objects_per_slab - 1;
                } else {
                    obj_index_in_slab -= 1;
                }
                assert_eq!(*v as usize, object_addr);
            }

            // 1 free, 1 full slabs
            assert_eq!(cache.free_slabs_list.iter().count(), 1);
            assert_eq!(cache.full_slabs_list.iter().count(), 1);
            // 46 free objects
            assert_eq!(
                (*cache
                    .free_slabs_list
                    .back()
                    .get()
                    .unwrap()
                    .data
                    .lock()
                    .get())
                .free_objects_list
                .iter()
                .count(),
                46
            );
            assert_eq!(
                (*cache
                    .free_slabs_list
                    .back()
                    .get()
                    .unwrap()
                    .data
                    .lock()
                    .get())
                .free_objects_number,
                46
            );

            // Check statistics
            assert_eq!(cache.statistics.free_slabs_number, 1);
            assert_eq!(cache.statistics.full_slabs_number, 1);
            assert_eq!(cache.statistics.allocated_objects_number, 100);
            assert_eq!(cache.statistics.free_objects_number, 46);

            // Free slabs and slab infos manualy (alloc test only)
            let allocated_slab_addrs = cache.memory_backend.allocated_slab_addrs.clone();
            let allocated_slab_infos = cache.memory_backend.allocated_slab_infos_addrs.clone();

            drop(cache);

            // Free slabs
            for addr in allocated_slab_addrs {
                let layout = Layout::from_size_align(SLAB_SIZE, PAGE_SIZE).unwrap();
                dealloc(addr as *mut u8, layout);
            }

            // Free slab infos
            for addr in allocated_slab_infos {
                let layout =
                    Layout::from_size_align(size_of::<SlabInfo>(), align_of::<SlabInfo>()).unwrap();
                dealloc(addr as *mut u8, layout);
            }
        }
    }

    // Allocations only
    // Large, slab size > page size
    // SlabInfo allocation
    // SlabInfo save
    #[test]
    fn _03_alloc_only_large_ss_neq_ps() {
        unsafe {
            const PAGE_SIZE: usize = 4096;
            const SLAB_SIZE: usize = 8192;
            const OBJECT_SIZE_TYPE: ObjectSizeType = ObjectSizeType::Large;

            struct TestObjectType16 {
                #[allow(unused)]
                a: [u64; 16 / 8],
            }
            assert_eq!(size_of::<TestObjectType16>(), 16);

            struct TestMemoryBackend {
                allocated_slab_addrs: Vec<usize>,
                allocated_slab_infos_addrs: Vec<usize>,
            }

            impl MemoryBackend for TestMemoryBackend {
                unsafe fn alloc_slab(&mut self, slab_size: usize, page_size: usize) -> *mut u8 {
                    assert_eq!(slab_size, SLAB_SIZE);
                    assert_eq!(page_size, PAGE_SIZE);
                    let layout = Layout::from_size_align(slab_size, page_size).unwrap();
                    let allocated_slab_ptr = alloc(layout);
                    self.allocated_slab_addrs.push(allocated_slab_ptr as usize);
                    allocated_slab_ptr
                }

                unsafe fn free_slab(
                    &mut self,
                    _slab_ptr: *mut u8,
                    _slab_size: usize,
                    _page_size: usize,
                ) {
                    unreachable!();
                }

                unsafe fn alloc_slab_info(&mut self) -> *mut SlabInfo {
                    let layout =
                        Layout::from_size_align(size_of::<SlabInfo>(), align_of::<SlabInfo>())
                            .unwrap();
                    let allocated_slab_info_ptr = alloc(layout);
                    self.allocated_slab_infos_addrs
                        .push(allocated_slab_info_ptr as usize);
                    allocated_slab_info_ptr.cast()
                }

                unsafe fn free_slab_info(&mut self, _slab_info_ptr: *mut SlabInfo) {
                    unreachable!();
                }

                unsafe fn save_slab_info_addr(
                    &mut self,
                    object_page_addr: usize,
                    slab_info_ptr: *mut SlabInfo,
                ) {
                    assert!(!slab_info_ptr.is_null());
                    assert!(slab_info_ptr.is_aligned());
                    assert_eq!(object_page_addr % PAGE_SIZE, 0);
                    // Get function not call's in this test
                }

                unsafe fn get_slab_info_addr(&mut self, _object_page_addr: usize) -> *mut SlabInfo {
                    unreachable!();
                }

                unsafe fn delete_slab_info_addr(&mut self, _page_addr: usize) {}
            }

            let test_memory_backend = TestMemoryBackend {
                allocated_slab_addrs: Vec::new(),
                allocated_slab_infos_addrs: Vec::new(),
            };

            // Create cache
            // 512 objects
            // [obj0, ..., obj511]
            let mut cache: Cache<TestObjectType16, TestMemoryBackend> =
                Cache::new(SLAB_SIZE, PAGE_SIZE, OBJECT_SIZE_TYPE, test_memory_backend).unwrap();
            assert_eq!(cache.objects_per_slab, 512);

            // Alloc 100 objects
            let mut allocated_ptrs = [null_mut(); 100];
            for v in allocated_ptrs.iter_mut() {
                *v = cache.alloc();
                assert!(!v.is_null());
                assert!(v.is_aligned());
            }
            // slab0
            // [obj99, ..., obj0] 100
            let mut obj_index_in_slab = cache.objects_per_slab - 1;
            for (i, v) in allocated_ptrs.iter().enumerate() {
                let slab_index = i / cache.objects_per_slab;
                let object_addr = cache.memory_backend.allocated_slab_addrs[slab_index]
                    + obj_index_in_slab * cache.object_size;
                if obj_index_in_slab == 0 {
                    obj_index_in_slab = cache.objects_per_slab - 1;
                } else {
                    obj_index_in_slab -= 1;
                }
                assert_eq!(*v as usize, object_addr);
            }

            // 1 free, 0 full slabs
            assert_eq!(cache.free_slabs_list.iter().count(), 1);
            assert_eq!(cache.full_slabs_list.iter().count(), 0);
            // 412 free objects
            assert_eq!(
                (*cache
                    .free_slabs_list
                    .back()
                    .get()
                    .unwrap()
                    .data
                    .lock()
                    .get())
                .free_objects_list
                .iter()
                .count(),
                412
            );
            assert_eq!(
                (*cache
                    .free_slabs_list
                    .back()
                    .get()
                    .unwrap()
                    .data
                    .lock()
                    .get())
                .free_objects_number,
                412
            );

            // Check statistics
            assert_eq!(cache.statistics.free_slabs_number, 1);
            assert_eq!(cache.statistics.full_slabs_number, 0);
            assert_eq!(cache.statistics.allocated_objects_number, 100);
            assert_eq!(cache.statistics.free_objects_number, 412);

            // Free slabs and slab infos manualy (alloc test only)
            let allocated_slab_addrs = cache.memory_backend.allocated_slab_addrs.clone();
            let allocated_slab_infos = cache.memory_backend.allocated_slab_infos_addrs.clone();

            drop(cache);

            // Free slabs
            for addr in allocated_slab_addrs {
                let layout = Layout::from_size_align(SLAB_SIZE, PAGE_SIZE).unwrap();
                dealloc(addr as *mut u8, layout);
            }

            // Free slab infos
            for addr in allocated_slab_infos {
                let layout =
                    Layout::from_size_align(size_of::<SlabInfo>(), align_of::<SlabInfo>()).unwrap();
                dealloc(addr as *mut u8, layout);
            }
        }
    }

    #[test]
    // Alloc and free
    // Small, slab size == page size
    // No SlabInfo allocation/free
    // No SlabInfo save/get
    // With random test
    fn _04_free_small_ss_eq_ps() {
        unsafe {
            const PAGE_SIZE: usize = 4096;
            const SLAB_SIZE: usize = 4096;
            const OBJECT_SIZE_TYPE: ObjectSizeType = ObjectSizeType::Small;

            #[repr(C)]
            struct TestObjectType512 {
                first_bytes: [u8; 128], // 128
                ptr_address: u64,       // 8
                last_bytes: [u8; 376],  // 376
            }
            assert_eq!(size_of::<TestObjectType512>(), 512);

            struct TestMemoryBackend {
                allocated_slab_addrs: Vec<usize>,
            }

            impl MemoryBackend for TestMemoryBackend {
                unsafe fn alloc_slab(&mut self, slab_size: usize, page_size: usize) -> *mut u8 {
                    assert_eq!(slab_size, SLAB_SIZE);
                    assert_eq!(page_size, PAGE_SIZE);
                    let layout = Layout::from_size_align(slab_size, page_size).unwrap();
                    let allocated_slab_ptr = alloc(layout);
                    assert!(!allocated_slab_ptr.is_null());
                    self.allocated_slab_addrs.push(allocated_slab_ptr as usize);
                    allocated_slab_ptr
                }

                unsafe fn free_slab(
                    &mut self,
                    slab_ptr: *mut u8,
                    slab_size: usize,
                    page_size: usize,
                ) {
                    let position = self
                        .allocated_slab_addrs
                        .iter()
                        .position(|addr| *addr == slab_ptr as usize)
                        .unwrap();
                    self.allocated_slab_addrs.remove(position);
                    assert_eq!(slab_size, SLAB_SIZE);
                    assert_eq!(page_size, PAGE_SIZE);
                    let layout = Layout::from_size_align(slab_size, page_size).unwrap();
                    dealloc(slab_ptr, layout);
                }

                unsafe fn alloc_slab_info(&mut self) -> *mut SlabInfo {
                    unreachable!();
                }

                unsafe fn free_slab_info(&mut self, _slab_info_ptr: *mut SlabInfo) {
                    unreachable!();
                }

                unsafe fn save_slab_info_addr(
                    &mut self,
                    _object_page_addr: usize,
                    _slab_info_ptr: *mut SlabInfo,
                ) {
                    unreachable!();
                }

                unsafe fn get_slab_info_addr(&mut self, _object_page_addr: usize) -> *mut SlabInfo {
                    unreachable!();
                }

                unsafe fn delete_slab_info_addr(&mut self, _page_addr: usize) {}
            }

            let test_memory_backend = TestMemoryBackend {
                allocated_slab_addrs: Vec::new(),
            };

            // Create cache
            // 7 objects
            // [obj0, obj1, obj2, obj3, obj4, obj5, obj6]
            let mut cache: Cache<TestObjectType512, TestMemoryBackend> =
                Cache::new(SLAB_SIZE, PAGE_SIZE, OBJECT_SIZE_TYPE, test_memory_backend).unwrap();
            assert_eq!(cache.objects_per_slab, 7);

            // Alloc 1
            let allocated_ptr = cache.alloc();
            assert!(!allocated_ptr.is_null());
            assert!(allocated_ptr.is_aligned());
            // Free 1
            cache.free(allocated_ptr);
            assert!(cache.free_slabs_list.is_empty());
            assert!(cache.memory_backend.allocated_slab_addrs.is_empty());

            // Alloc first slab particaly
            let mut first_slab_ptrs = vec![null_mut(); cache.objects_per_slab - 1];
            for v in first_slab_ptrs.iter_mut() {
                *v = cache.alloc();
                assert!(!v.is_null());
                assert!(v.is_aligned());
            }

            // 1 free slab, 0 full slab
            assert_eq!(cache.free_slabs_list.iter().count(), 1);
            assert_eq!(cache.full_slabs_list.iter().count(), 0);

            // Alloc last object
            first_slab_ptrs.push(cache.alloc());
            assert!(!first_slab_ptrs.last().unwrap().is_null());
            assert!(first_slab_ptrs.last().unwrap().is_aligned());

            let first_slab_ptrs_copy = first_slab_ptrs.clone();

            // 0 free slabs, 1 full
            assert_eq!(cache.free_slabs_list.iter().count(), 0);
            assert_eq!(cache.full_slabs_list.iter().count(), 1);

            // Mix addresses
            first_slab_ptrs.shuffle(&mut rand::thread_rng());

            // Free all objects except one
            let len = first_slab_ptrs.len() - 1;
            for _ in 0..len {
                cache.free(first_slab_ptrs.pop().unwrap());
            }
            // 1 free slabs, 0 full
            assert_eq!(cache.free_slabs_list.iter().count(), 1);
            assert_eq!(cache.full_slabs_list.iter().count(), 0);

            // Alloc again all objects
            for _ in 0..len {
                first_slab_ptrs.push(cache.alloc());
            }
            // Compare first slab ptrs copy and current
            for a in first_slab_ptrs.iter() {
                assert!(first_slab_ptrs_copy.iter().any(|a_copy| { a == a_copy }));
            }
            let hs: HashSet<*mut TestObjectType512> =
                first_slab_ptrs_copy.iter().copied().collect();
            assert_eq!(hs.len(), first_slab_ptrs_copy.len());

            // 0 free slabs, 1 full
            assert_eq!(cache.free_slabs_list.iter().count(), 0);
            assert_eq!(cache.full_slabs_list.iter().count(), 1);

            // Alloc 0.5 slab
            let mut second_slab_ptrs = Vec::new();
            for _ in 0..cache.objects_per_slab / 2 {
                second_slab_ptrs.push(cache.alloc());
                assert!(!second_slab_ptrs.last().unwrap().is_null());
                assert!(second_slab_ptrs.last().unwrap().is_aligned());
            }

            // 1 free slabs, 1 full slabs
            assert_eq!(cache.free_slabs_list.iter().count(), 1);
            assert_eq!(cache.full_slabs_list.iter().count(), 1);

            // Free first slab
            first_slab_ptrs.shuffle(&mut rand::thread_rng());
            for v in first_slab_ptrs.iter() {
                cache.free(*v);
            }

            // 1 free slabs, 0 full slabs
            assert_eq!(cache.free_slabs_list.iter().count(), 1);
            assert_eq!(cache.full_slabs_list.iter().count(), 0);

            // Free second slab
            second_slab_ptrs.shuffle(&mut rand::thread_rng());
            for v in second_slab_ptrs.iter() {
                cache.free(*v);
            }

            // 0 free slabs, 0 full slabs
            assert_eq!(cache.free_slabs_list.iter().count(), 0);
            assert_eq!(cache.full_slabs_list.iter().count(), 0);
            assert_eq!(cache.memory_backend.allocated_slab_addrs.len(), 0);

            // Random test

            // Random number of test
            for _ in 0..rand::thread_rng().gen_range(20..=40) {
                let mut allocated_ptrs = Vec::new();

                for _ in 10..=20 {
                    // Alloc or free
                    if rand::thread_rng().gen_bool(0.5) {
                        // Alloc random number of objects
                        for _ in 0..rand::thread_rng().gen_range(20..100) {
                            let allocated_ptr = cache.alloc();
                            assert!(!allocated_ptr.is_null());
                            assert!(allocated_ptr.is_aligned());
                            allocated_ptrs.push(allocated_ptr);
                            // Fill allocated memory
                            let random_byte: u8 = rand::thread_rng().gen_range(0u8..=255u8);
                            (*allocated_ptr).first_bytes = [random_byte; 128];
                            (*allocated_ptr).ptr_address = allocated_ptr as u64;
                            (*allocated_ptr).last_bytes = [random_byte; 376];
                        }
                    } else {
                        allocated_ptrs.shuffle(&mut rand::thread_rng());
                        // Free random number of objects
                        for _ in 0..rand::thread_rng().gen_range(0..=allocated_ptrs.len()) {
                            let freed_ptr = allocated_ptrs.pop().unwrap();
                            // Check memory
                            assert_eq!(
                                (*freed_ptr).first_bytes,
                                [(*freed_ptr).first_bytes[0]; 128]
                            );
                            assert_eq!((*freed_ptr).ptr_address, freed_ptr as u64);
                            assert_eq!((*freed_ptr).last_bytes, [(*freed_ptr).last_bytes[0]; 376]);
                            cache.free(freed_ptr);
                        }
                    }
                }

                // All addresses are unique
                let hs: HashSet<_> = HashSet::from_iter(allocated_ptrs.clone().into_iter());
                assert_eq!(hs.len(), allocated_ptrs.len());

                // Check statistics
                assert_eq!(
                    cache.statistics.allocated_objects_number,
                    allocated_ptrs.len()
                );
                let mut free_objects_counter = 0;
                for free_slab_info in cache.free_slabs_list.iter() {
                    free_objects_counter += (*free_slab_info.data.lock().get()).free_objects_number;
                }
                assert_eq!(cache.statistics.free_objects_number, free_objects_counter);
                assert_eq!(
                    cache.statistics.full_slabs_number,
                    cache.full_slabs_list.iter().count()
                );
                assert_eq!(
                    cache.statistics.free_slabs_number,
                    cache.free_slabs_list.iter().count()
                );

                // Free all objects
                allocated_ptrs.shuffle(&mut rand::thread_rng());
                for v in allocated_ptrs.into_iter() {
                    cache.free(v);
                }
                assert_eq!(cache.memory_backend.allocated_slab_addrs.len(), 0);
            }

            assert!(cache.free_slabs_list.is_empty());
            assert!(cache.full_slabs_list.is_empty());
            assert_eq!(cache.memory_backend.allocated_slab_addrs.len(), 0);

            // Check statistics
            assert_eq!(cache.statistics.free_slabs_number, 0);
            assert_eq!(cache.statistics.full_slabs_number, 0);
            assert_eq!(cache.statistics.allocated_objects_number, 0);
            assert_eq!(cache.statistics.free_objects_number, 0);
        }
    }

    // Alloc and free
    // Small, slab size > page size
    // No SlabInfo allocation/free
    // SlabInfo save/get
    // With random test
    #[test]
    fn _05_free_small_ss_neq_ps() {
        unsafe {
            const PAGE_SIZE: usize = 4096;
            const SLAB_SIZE: usize = 8192;
            const OBJECT_SIZE_TYPE: ObjectSizeType = ObjectSizeType::Small;

            #[repr(C)]
            struct TestObjectType512 {
                first_bytes: [u8; 128], // 128
                ptr_address: u64,       // 8
                last_bytes: [u8; 376],  // 376
            }
            assert_eq!(size_of::<TestObjectType512>(), 512);

            struct TestMemoryBackend {
                allocated_slab_addrs: Vec<usize>,
                ht_saved_slab_infos: HashMap<usize, *mut SlabInfo>,
                // Counts save/get calls
                ht_save_get_calls_counter: HashMap<*mut SlabInfo, usize>,
            }

            impl MemoryBackend for TestMemoryBackend {
                unsafe fn alloc_slab(&mut self, slab_size: usize, page_size: usize) -> *mut u8 {
                    assert_eq!(slab_size, SLAB_SIZE);
                    assert_eq!(page_size, PAGE_SIZE);
                    let layout = Layout::from_size_align(slab_size, page_size).unwrap();
                    let allocated_slab_ptr = alloc(layout);
                    assert!(!allocated_slab_ptr.is_null());
                    self.allocated_slab_addrs.push(allocated_slab_ptr as usize);
                    allocated_slab_ptr
                }

                unsafe fn free_slab(
                    &mut self,
                    slab_ptr: *mut u8,
                    slab_size: usize,
                    page_size: usize,
                ) {
                    let position = self
                        .allocated_slab_addrs
                        .iter()
                        .position(|addr| *addr == slab_ptr as usize)
                        .unwrap();
                    self.allocated_slab_addrs.remove(position);
                    assert_eq!(slab_size, SLAB_SIZE);
                    assert_eq!(page_size, PAGE_SIZE);
                    let layout = Layout::from_size_align(slab_size, page_size).unwrap();
                    dealloc(slab_ptr, layout);
                }

                unsafe fn alloc_slab_info(&mut self) -> *mut SlabInfo {
                    unreachable!();
                }

                unsafe fn free_slab_info(&mut self, _slab_info_ptr: *mut SlabInfo) {
                    unreachable!();
                }

                unsafe fn save_slab_info_addr(
                    &mut self,
                    object_page_addr: usize,
                    slab_info_ptr: *mut SlabInfo,
                ) {
                    assert!(!slab_info_ptr.is_null());
                    assert!(slab_info_ptr.is_aligned());
                    assert_eq!(object_page_addr % PAGE_SIZE, 0);
                    self.ht_saved_slab_infos
                        .insert(object_page_addr, slab_info_ptr);
                    if let Some(counter) = self.ht_save_get_calls_counter.get_mut(&slab_info_ptr) {
                        *counter += 1;
                    } else {
                        self.ht_save_get_calls_counter.insert(slab_info_ptr, 1);
                    }
                }

                unsafe fn get_slab_info_addr(&mut self, object_page_addr: usize) -> *mut SlabInfo {
                    let slab_info_ptr = *self.ht_saved_slab_infos.get(&object_page_addr).unwrap();
                    let counter = self
                        .ht_save_get_calls_counter
                        .get_mut(&slab_info_ptr)
                        .unwrap();
                    *counter -= 1;
                    slab_info_ptr
                }

                unsafe fn delete_slab_info_addr(&mut self, page_addr: usize) {
                    self.ht_saved_slab_infos.remove(&page_addr);
                }
            }

            let test_memory_backend = TestMemoryBackend {
                allocated_slab_addrs: Vec::new(),
                ht_saved_slab_infos: HashMap::new(),
                ht_save_get_calls_counter: HashMap::new(),
            };

            // Create cache
            // 15 objects
            let mut cache: Cache<TestObjectType512, TestMemoryBackend> =
                Cache::new(SLAB_SIZE, PAGE_SIZE, OBJECT_SIZE_TYPE, test_memory_backend).unwrap();
            assert_eq!(cache.objects_per_slab, 15);

            // Alloc 1
            let allocated_ptr = cache.alloc();
            assert!(!allocated_ptr.is_null());
            assert!(allocated_ptr.is_aligned());
            // Free 1
            cache.free(allocated_ptr);
            assert!(cache.free_slabs_list.is_empty());
            assert!(cache.memory_backend.allocated_slab_addrs.is_empty());
            assert!(cache.memory_backend.ht_saved_slab_infos.is_empty());

            // Alloc first slab particaly
            let mut first_slab_ptrs = vec![null_mut(); cache.objects_per_slab - 1];
            for v in first_slab_ptrs.iter_mut() {
                *v = cache.alloc();
                assert!(!v.is_null());
                assert!(v.is_aligned());
            }

            // 1 free slab, 0 full slab
            assert_eq!(cache.free_slabs_list.iter().count(), 1);
            assert_eq!(cache.full_slabs_list.iter().count(), 0);

            // Alloc last object
            first_slab_ptrs.push(cache.alloc());
            assert!(!first_slab_ptrs.last().unwrap().is_null());
            assert!(first_slab_ptrs.last().unwrap().is_aligned());

            let first_slab_ptrs_copy = first_slab_ptrs.clone();

            // 0 free slabs, 1 full
            assert_eq!(cache.free_slabs_list.iter().count(), 0);
            assert_eq!(cache.full_slabs_list.iter().count(), 1);

            // Mix addresses
            first_slab_ptrs.shuffle(&mut rand::thread_rng());

            // Free all objects except one
            let len = first_slab_ptrs.len() - 1;
            for _ in 0..len {
                cache.free(first_slab_ptrs.pop().unwrap());
            }
            // 1 free slabs, 0 full
            assert_eq!(cache.free_slabs_list.iter().count(), 1);
            assert_eq!(cache.full_slabs_list.iter().count(), 0);

            // Alloc again all objects
            for _ in 0..len {
                first_slab_ptrs.push(cache.alloc());
            }
            // Compare first slab ptrs copy and current
            for a in first_slab_ptrs.iter() {
                assert!(first_slab_ptrs_copy.iter().any(|a_copy| { a == a_copy }));
            }
            let hs: HashSet<*mut TestObjectType512> =
                first_slab_ptrs_copy.iter().copied().collect();
            assert_eq!(hs.len(), first_slab_ptrs_copy.len());

            // 0 free slabs, 1 full
            assert_eq!(cache.free_slabs_list.iter().count(), 0);
            assert_eq!(cache.full_slabs_list.iter().count(), 1);

            // Alloc 0.5 slab
            let mut second_slab_ptrs = Vec::new();
            for _ in 0..cache.objects_per_slab / 2 {
                second_slab_ptrs.push(cache.alloc());
                assert!(!second_slab_ptrs.last().unwrap().is_null());
                assert!(second_slab_ptrs.last().unwrap().is_aligned());
            }

            // 1 free slabs, 1 full slabs
            assert_eq!(cache.free_slabs_list.iter().count(), 1);
            assert_eq!(cache.full_slabs_list.iter().count(), 1);

            // Free first slab
            first_slab_ptrs.shuffle(&mut rand::thread_rng());
            for v in first_slab_ptrs.iter() {
                cache.free(*v);
            }

            // 1 free slabs, 0 full slabs
            assert_eq!(cache.free_slabs_list.iter().count(), 1);
            assert_eq!(cache.full_slabs_list.iter().count(), 0);

            // Free second slab
            second_slab_ptrs.shuffle(&mut rand::thread_rng());
            for v in second_slab_ptrs.iter() {
                cache.free(*v);
            }

            // All memory free
            // 0 free slabs, 0 full slabs
            assert_eq!(cache.free_slabs_list.iter().count(), 0);
            assert_eq!(cache.full_slabs_list.iter().count(), 0);
            assert_eq!(cache.memory_backend.allocated_slab_addrs.len(), 0);
            assert!(cache.memory_backend.ht_saved_slab_infos.is_empty());

            // Save calls count == get calls count
            assert!(cache
                .memory_backend
                .ht_save_get_calls_counter
                .iter()
                .all(|v| *v.1 == 0));

            // Random test

            // Random number of test
            for _ in 0..rand::thread_rng().gen_range(20..=40) {
                let mut allocated_ptrs = Vec::new();

                for _ in 10..=20 {
                    // Alloc or free
                    if rand::thread_rng().gen_bool(0.5) {
                        // Alloc random number of objects
                        for _ in 0..rand::thread_rng().gen_range(20..100) {
                            let allocated_ptr = cache.alloc();
                            assert!(!allocated_ptr.is_null());
                            assert!(allocated_ptr.is_aligned());
                            allocated_ptrs.push(allocated_ptr);
                            // Fill allocated memory
                            let random_byte: u8 = rand::thread_rng().gen_range(0u8..=255u8);
                            (*allocated_ptr).first_bytes = [random_byte; 128];
                            (*allocated_ptr).ptr_address = allocated_ptr as u64;
                            (*allocated_ptr).last_bytes = [random_byte; 376];
                        }
                    } else {
                        allocated_ptrs.shuffle(&mut rand::thread_rng());
                        // Free random number of objects
                        for _ in 0..rand::thread_rng().gen_range(0..=allocated_ptrs.len()) {
                            let freed_ptr = allocated_ptrs.pop().unwrap();
                            // Check memory
                            assert_eq!(
                                (*freed_ptr).first_bytes,
                                [(*freed_ptr).first_bytes[0]; 128]
                            );
                            assert_eq!((*freed_ptr).ptr_address, freed_ptr as u64);
                            assert_eq!((*freed_ptr).last_bytes, [(*freed_ptr).last_bytes[0]; 376]);
                            cache.free(freed_ptr);
                        }
                    }
                }

                // All addresses are unique
                let hs: HashSet<_> = HashSet::from_iter(allocated_ptrs.clone().into_iter());
                assert_eq!(hs.len(), allocated_ptrs.len());
                assert_eq!(
                    cache.statistics.allocated_objects_number,
                    allocated_ptrs.len()
                );

                // Check statistics
                assert_eq!(
                    cache.statistics.allocated_objects_number,
                    allocated_ptrs.len()
                );
                let mut free_objects_counter = 0;
                for free_slab_info in cache.free_slabs_list.iter() {
                    free_objects_counter += (*free_slab_info.data.lock().get()).free_objects_number;
                }
                assert_eq!(cache.statistics.free_objects_number, free_objects_counter);
                assert_eq!(
                    cache.statistics.full_slabs_number,
                    cache.full_slabs_list.iter().count()
                );
                assert_eq!(
                    cache.statistics.free_slabs_number,
                    cache.free_slabs_list.iter().count()
                );

                // Free all objects
                allocated_ptrs.shuffle(&mut rand::thread_rng());
                for v in allocated_ptrs.into_iter() {
                    cache.free(v);
                }
                assert_eq!(cache.memory_backend.allocated_slab_addrs.len(), 0);
            }

            assert!(cache.free_slabs_list.is_empty());
            assert!(cache.full_slabs_list.is_empty());
            assert_eq!(cache.memory_backend.allocated_slab_addrs.len(), 0);

            // Check statistics
            assert_eq!(cache.statistics.free_slabs_number, 0);
            assert_eq!(cache.statistics.full_slabs_number, 0);
            assert_eq!(cache.statistics.allocated_objects_number, 0);
            assert_eq!(cache.statistics.free_objects_number, 0);
        }
    }

    // Alloc and free
    // Large, slab size == page size
    // SlabInfo allocation/free
    // SlabInfo save/get
    // With random test
    #[test]
    fn _06_free_large_ss_eq_ps() {
        unsafe {
            const PAGE_SIZE: usize = 4096;
            const SLAB_SIZE: usize = 4096;
            const OBJECT_SIZE_TYPE: ObjectSizeType = ObjectSizeType::Large;

            #[repr(C)]
            struct TestObjectType512 {
                first_bytes: [u8; 128], // 128
                ptr_address: u64,       // 8
                last_bytes: [u8; 376],  // 376
            }
            assert_eq!(size_of::<TestObjectType512>(), 512);

            struct TestMemoryBackend {
                allocated_slab_addrs: Vec<usize>,
                allocated_slab_info_addrs: Vec<usize>,
                ht_saved_slab_infos: HashMap<usize, *mut SlabInfo>,
                // Counts save/get calls
                // In this test, the number of SlabInfo saves will always be less,
                // because there is an internal optimization in the code:
                // it makes no sense to save SlabInfo many times, since there is only one page in the slab.
                // Only 1 save call while first object allocates
            }

            impl MemoryBackend for TestMemoryBackend {
                unsafe fn alloc_slab(&mut self, slab_size: usize, page_size: usize) -> *mut u8 {
                    assert_eq!(slab_size, SLAB_SIZE);
                    assert_eq!(page_size, PAGE_SIZE);
                    let layout = Layout::from_size_align(slab_size, page_size).unwrap();
                    let allocated_slab_ptr = alloc(layout);
                    assert!(!allocated_slab_ptr.is_null());
                    self.allocated_slab_addrs.push(allocated_slab_ptr as usize);
                    allocated_slab_ptr
                }

                unsafe fn free_slab(
                    &mut self,
                    slab_ptr: *mut u8,
                    slab_size: usize,
                    page_size: usize,
                ) {
                    let position = self
                        .allocated_slab_addrs
                        .iter()
                        .position(|addr| *addr == slab_ptr as usize)
                        .unwrap();
                    self.allocated_slab_addrs.remove(position);
                    assert_eq!(slab_size, SLAB_SIZE);
                    assert_eq!(page_size, PAGE_SIZE);
                    let layout = Layout::from_size_align(slab_size, page_size).unwrap();
                    dealloc(slab_ptr, layout);
                }

                unsafe fn alloc_slab_info(&mut self) -> *mut SlabInfo {
                    let layout = Layout::new::<SlabInfo>();
                    let allocated_ptr: *mut SlabInfo = alloc(layout).cast();
                    assert!(!allocated_ptr.is_null());
                    self.allocated_slab_info_addrs.push(allocated_ptr as usize);
                    allocated_ptr
                }

                unsafe fn free_slab_info(&mut self, slab_info_ptr: *mut SlabInfo) {
                    assert!(!slab_info_ptr.is_null());
                    assert!(slab_info_ptr.is_aligned());
                    let position = self
                        .allocated_slab_info_addrs
                        .iter()
                        .position(|addr| *addr == slab_info_ptr as usize)
                        .unwrap();
                    assert!(self
                        .ht_saved_slab_infos
                        .iter()
                        .any(|(_, value)| *value == slab_info_ptr));
                    self.allocated_slab_info_addrs.remove(position);
                    let layout = Layout::new::<SlabInfo>();
                    dealloc(slab_info_ptr.cast(), layout);
                }

                unsafe fn save_slab_info_addr(
                    &mut self,
                    object_page_addr: usize,
                    slab_info_ptr: *mut SlabInfo,
                ) {
                    assert!(!slab_info_ptr.is_null());
                    assert!(slab_info_ptr.is_aligned());
                    assert_eq!(object_page_addr % PAGE_SIZE, 0);
                    self.ht_saved_slab_infos
                        .insert(object_page_addr, slab_info_ptr);
                }

                unsafe fn get_slab_info_addr(&mut self, object_page_addr: usize) -> *mut SlabInfo {
                    let slab_info_ptr = *self.ht_saved_slab_infos.get(&object_page_addr).unwrap();
                    slab_info_ptr
                }

                unsafe fn delete_slab_info_addr(&mut self, page_addr: usize) {
                    assert!(self.ht_saved_slab_infos.remove(&page_addr).is_some());
                }
            }

            let test_memory_backend = TestMemoryBackend {
                allocated_slab_addrs: Vec::new(),
                allocated_slab_info_addrs: Vec::new(),
                ht_saved_slab_infos: HashMap::new(),
            };

            // Create cache
            // 8 objects
            let mut cache: Cache<TestObjectType512, TestMemoryBackend> =
                Cache::new(SLAB_SIZE, PAGE_SIZE, OBJECT_SIZE_TYPE, test_memory_backend).unwrap();
            assert_eq!(cache.objects_per_slab, 8);

            // Alloc 1
            let allocated_ptr = cache.alloc();
            assert!(!allocated_ptr.is_null());
            assert!(allocated_ptr.is_aligned());
            // Free 1
            cache.free(allocated_ptr);
            assert!(cache.free_slabs_list.is_empty());
            assert!(cache.memory_backend.allocated_slab_addrs.is_empty());
            assert!(cache.memory_backend.ht_saved_slab_infos.is_empty());

            // Alloc first slab particaly
            let mut first_slab_ptrs = vec![null_mut(); cache.objects_per_slab - 1];
            for v in first_slab_ptrs.iter_mut() {
                *v = cache.alloc();
                assert!(!v.is_null());
                assert!(v.is_aligned());
            }

            // 1 free slab, 0 full slab
            assert_eq!(cache.free_slabs_list.iter().count(), 1);
            assert_eq!(cache.full_slabs_list.iter().count(), 0);

            // Alloc last object
            first_slab_ptrs.push(cache.alloc());
            assert!(!first_slab_ptrs.last().unwrap().is_null());
            assert!(first_slab_ptrs.last().unwrap().is_aligned());

            let first_slab_ptrs_copy = first_slab_ptrs.clone();

            // 0 free slabs, 1 full
            assert_eq!(cache.free_slabs_list.iter().count(), 0);
            assert_eq!(cache.full_slabs_list.iter().count(), 1);

            // Mix addresses
            first_slab_ptrs.shuffle(&mut rand::thread_rng());

            // Free all objects except one
            let len = first_slab_ptrs.len() - 1;
            for _ in 0..len {
                cache.free(first_slab_ptrs.pop().unwrap());
            }
            // 1 free slabs, 0 full
            assert_eq!(cache.free_slabs_list.iter().count(), 1);
            assert_eq!(cache.full_slabs_list.iter().count(), 0);

            // Alloc again all objects
            for _ in 0..len {
                first_slab_ptrs.push(cache.alloc());
            }
            // Compare first slab ptrs copy and current
            for a in first_slab_ptrs.iter() {
                assert!(first_slab_ptrs_copy.iter().any(|a_copy| { a == a_copy }));
            }
            let hs: HashSet<*mut TestObjectType512> =
                first_slab_ptrs_copy.iter().copied().collect();
            assert_eq!(hs.len(), first_slab_ptrs_copy.len());

            // 0 free slabs, 1 full
            assert_eq!(cache.free_slabs_list.iter().count(), 0);
            assert_eq!(cache.full_slabs_list.iter().count(), 1);

            // Alloc 0.5 slab
            let mut second_slab_ptrs = Vec::new();
            for _ in 0..cache.objects_per_slab / 2 {
                second_slab_ptrs.push(cache.alloc());
                assert!(!second_slab_ptrs.last().unwrap().is_null());
                assert!(second_slab_ptrs.last().unwrap().is_aligned());
            }

            // 1 free slabs, 1 full slabs
            assert_eq!(cache.free_slabs_list.iter().count(), 1);
            assert_eq!(cache.full_slabs_list.iter().count(), 1);

            // Free first slab
            first_slab_ptrs.shuffle(&mut rand::thread_rng());
            for v in first_slab_ptrs.iter() {
                cache.free(*v);
            }

            // 1 free slabs, 0 full slabs
            assert_eq!(cache.free_slabs_list.iter().count(), 1);
            assert_eq!(cache.full_slabs_list.iter().count(), 0);

            // Free second slab
            second_slab_ptrs.shuffle(&mut rand::thread_rng());
            for v in second_slab_ptrs.iter() {
                cache.free(*v);
            }

            // All memory free
            // 0 free slabs, 0 full slabs
            assert_eq!(cache.free_slabs_list.iter().count(), 0);
            assert_eq!(cache.full_slabs_list.iter().count(), 0);
            assert_eq!(cache.memory_backend.allocated_slab_addrs.len(), 0);
            assert_eq!(cache.memory_backend.allocated_slab_info_addrs.len(), 0);
            assert!(cache.memory_backend.ht_saved_slab_infos.is_empty());

            // Random test

            // Random number of test
            for _ in 0..rand::thread_rng().gen_range(20..=40) {
                let mut allocated_ptrs = Vec::new();

                for _ in 10..=20 {
                    // Alloc or free
                    if rand::thread_rng().gen_bool(0.5) {
                        // Alloc random number of objects
                        for _ in 0..rand::thread_rng().gen_range(20..100) {
                            let allocated_ptr = cache.alloc();
                            assert!(!allocated_ptr.is_null());
                            assert!(allocated_ptr.is_aligned());
                            allocated_ptrs.push(allocated_ptr);
                            // Fill allocated memory
                            let random_byte: u8 = rand::thread_rng().gen_range(0u8..=255u8);
                            (*allocated_ptr).first_bytes = [random_byte; 128];
                            (*allocated_ptr).ptr_address = allocated_ptr as u64;
                            (*allocated_ptr).last_bytes = [random_byte; 376];
                        }
                    } else {
                        allocated_ptrs.shuffle(&mut rand::thread_rng());
                        // Free random number of objects
                        for _ in 0..rand::thread_rng().gen_range(0..=allocated_ptrs.len()) {
                            let freed_ptr = allocated_ptrs.pop().unwrap();
                            // Check memory
                            assert_eq!(
                                (*freed_ptr).first_bytes,
                                [(*freed_ptr).first_bytes[0]; 128]
                            );
                            assert_eq!((*freed_ptr).ptr_address, freed_ptr as u64);
                            assert_eq!((*freed_ptr).last_bytes, [(*freed_ptr).last_bytes[0]; 376]);
                            cache.free(freed_ptr);
                        }
                    }
                }

                // All addresses are unique
                let hs: HashSet<_> = HashSet::from_iter(allocated_ptrs.clone().into_iter());
                assert_eq!(hs.len(), allocated_ptrs.len());
                assert_eq!(
                    cache.statistics.allocated_objects_number,
                    allocated_ptrs.len()
                );

                // Check statistics
                assert_eq!(
                    cache.statistics.allocated_objects_number,
                    allocated_ptrs.len()
                );
                let mut free_objects_counter = 0;
                for free_slab_info in cache.free_slabs_list.iter() {
                    free_objects_counter += (*free_slab_info.data.lock().get()).free_objects_number;
                }
                assert_eq!(cache.statistics.free_objects_number, free_objects_counter);
                assert_eq!(
                    cache.statistics.full_slabs_number,
                    cache.full_slabs_list.iter().count()
                );
                assert_eq!(
                    cache.statistics.free_slabs_number,
                    cache.free_slabs_list.iter().count()
                );

                // Free all objects
                allocated_ptrs.shuffle(&mut rand::thread_rng());
                for v in allocated_ptrs.into_iter() {
                    cache.free(v);
                }
                assert_eq!(cache.memory_backend.allocated_slab_addrs.len(), 0);
            }

            assert!(cache.free_slabs_list.is_empty());
            assert!(cache.full_slabs_list.is_empty());
            assert_eq!(cache.memory_backend.allocated_slab_addrs.len(), 0);
            assert_eq!(cache.memory_backend.allocated_slab_info_addrs.len(), 0);
            assert!(cache.memory_backend.ht_saved_slab_infos.is_empty());

            // Check statistics
            assert_eq!(cache.statistics.free_slabs_number, 0);
            assert_eq!(cache.statistics.full_slabs_number, 0);
            assert_eq!(cache.statistics.allocated_objects_number, 0);
            assert_eq!(cache.statistics.free_objects_number, 0);
        }
    }

    // Alloc and free
    // Large, slab size >= page size
    // SlabInfo allocation/free
    // SlabInfo save/get
    // With random test
    #[test]
    fn _07_free_large_ss_neq_ps() {
        unsafe {
            const PAGE_SIZE: usize = 4096;
            const SLAB_SIZE: usize = 8192;
            const OBJECT_SIZE_TYPE: ObjectSizeType = ObjectSizeType::Large;

            #[repr(C)]
            struct TestObjectType256 {
                first_bytes: [u8; 128], // 128
                ptr_address: u64,       // 8
                last_bytes: [u8; 120],  // 120
            }
            assert_eq!(size_of::<TestObjectType256>(), 256);

            struct TestMemoryBackend {
                allocated_slab_addrs: Vec<usize>,
                allocated_slab_info_addrs: Vec<usize>,
                ht_saved_slab_infos: HashMap<usize, *mut SlabInfo>,
                // Counts save/get calls
                // In this test, the number of SlabInfo saves will always be less,
                // because there is an internal optimization in the code:
                // it makes no sense to save SlabInfo many times, since there is only one page in the slab.
                // Obly 1 save call while first object allocates
            }

            impl MemoryBackend for TestMemoryBackend {
                unsafe fn alloc_slab(&mut self, slab_size: usize, page_size: usize) -> *mut u8 {
                    assert_eq!(slab_size, SLAB_SIZE);
                    assert_eq!(page_size, PAGE_SIZE);
                    let layout = Layout::from_size_align(slab_size, page_size).unwrap();
                    let allocated_slab_ptr = alloc(layout);
                    assert!(!allocated_slab_ptr.is_null());
                    self.allocated_slab_addrs.push(allocated_slab_ptr as usize);
                    allocated_slab_ptr
                }

                unsafe fn free_slab(
                    &mut self,
                    slab_ptr: *mut u8,
                    slab_size: usize,
                    page_size: usize,
                ) {
                    let position = self
                        .allocated_slab_addrs
                        .iter()
                        .position(|addr| *addr == slab_ptr as usize)
                        .unwrap();
                    self.allocated_slab_addrs.remove(position);
                    assert_eq!(slab_size, SLAB_SIZE);
                    assert_eq!(page_size, PAGE_SIZE);
                    let layout = Layout::from_size_align(slab_size, page_size).unwrap();
                    dealloc(slab_ptr, layout);
                }

                unsafe fn alloc_slab_info(&mut self) -> *mut SlabInfo {
                    let layout = Layout::new::<SlabInfo>();
                    let allocated_ptr: *mut SlabInfo = alloc(layout).cast();
                    assert!(!allocated_ptr.is_null());
                    self.allocated_slab_info_addrs.push(allocated_ptr as usize);
                    allocated_ptr
                }

                unsafe fn free_slab_info(&mut self, slab_info_ptr: *mut SlabInfo) {
                    assert!(!slab_info_ptr.is_null());
                    assert!(slab_info_ptr.is_aligned());
                    let position = self
                        .allocated_slab_info_addrs
                        .iter()
                        .position(|addr| *addr == slab_info_ptr as usize)
                        .unwrap();
                    assert!(self
                        .ht_saved_slab_infos
                        .iter()
                        .any(|(_, value)| *value == slab_info_ptr));
                    self.allocated_slab_info_addrs.remove(position);
                    let layout = Layout::new::<SlabInfo>();
                    dealloc(slab_info_ptr.cast(), layout);
                }

                unsafe fn save_slab_info_addr(
                    &mut self,
                    object_page_addr: usize,
                    slab_info_ptr: *mut SlabInfo,
                ) {
                    assert!(!slab_info_ptr.is_null());
                    assert!(slab_info_ptr.is_aligned());
                    assert_eq!(object_page_addr % PAGE_SIZE, 0);
                    self.ht_saved_slab_infos
                        .insert(object_page_addr, slab_info_ptr);
                }

                unsafe fn get_slab_info_addr(&mut self, object_page_addr: usize) -> *mut SlabInfo {
                    let slab_info_ptr = *self.ht_saved_slab_infos.get(&object_page_addr).unwrap();
                    slab_info_ptr
                }

                unsafe fn delete_slab_info_addr(&mut self, page_addr: usize) {
                    self.ht_saved_slab_infos.remove(&page_addr);
                }
            }

            let test_memory_backend = TestMemoryBackend {
                allocated_slab_addrs: Vec::new(),
                allocated_slab_info_addrs: Vec::new(),
                ht_saved_slab_infos: HashMap::new(),
            };

            // Create cache
            // 8 objects
            let mut cache: Cache<TestObjectType256, TestMemoryBackend> =
                Cache::new(SLAB_SIZE, PAGE_SIZE, OBJECT_SIZE_TYPE, test_memory_backend).unwrap();
            assert_eq!(cache.objects_per_slab, 32);

            // Alloc 1
            let allocated_ptr = cache.alloc();
            assert!(!allocated_ptr.is_null());
            assert!(allocated_ptr.is_aligned());
            // Free 1
            cache.free(allocated_ptr);
            assert!(cache.free_slabs_list.is_empty());
            assert!(cache.memory_backend.allocated_slab_addrs.is_empty());
            assert!(cache.memory_backend.ht_saved_slab_infos.is_empty());

            // Alloc first slab particaly
            let mut first_slab_ptrs = vec![null_mut(); cache.objects_per_slab - 1];
            for v in first_slab_ptrs.iter_mut() {
                *v = cache.alloc();
                assert!(!v.is_null());
                assert!(v.is_aligned());
            }

            // 1 free slab, 0 full slab
            assert_eq!(cache.free_slabs_list.iter().count(), 1);
            assert_eq!(cache.full_slabs_list.iter().count(), 0);

            // Alloc last object
            first_slab_ptrs.push(cache.alloc());
            assert!(!first_slab_ptrs.last().unwrap().is_null());
            assert!(first_slab_ptrs.last().unwrap().is_aligned());

            let first_slab_ptrs_copy = first_slab_ptrs.clone();

            // 0 free slabs, 1 full
            assert_eq!(cache.free_slabs_list.iter().count(), 0);
            assert_eq!(cache.full_slabs_list.iter().count(), 1);

            // Mix addresses
            first_slab_ptrs.shuffle(&mut rand::thread_rng());

            // Free all objects except one
            let len = first_slab_ptrs.len() - 1;
            for _ in 0..len {
                cache.free(first_slab_ptrs.pop().unwrap());
            }
            // 1 free slabs, 0 full
            assert_eq!(cache.free_slabs_list.iter().count(), 1);
            assert_eq!(cache.full_slabs_list.iter().count(), 0);

            // Alloc again all objects
            for _ in 0..len {
                first_slab_ptrs.push(cache.alloc());
            }
            // Compare first slab ptrs copy and current
            for a in first_slab_ptrs.iter() {
                assert!(first_slab_ptrs_copy.iter().any(|a_copy| { a == a_copy }));
            }
            let hs: HashSet<*mut TestObjectType256> =
                first_slab_ptrs_copy.iter().copied().collect();
            assert_eq!(hs.len(), first_slab_ptrs_copy.len());

            // 0 free slabs, 1 full
            assert_eq!(cache.free_slabs_list.iter().count(), 0);
            assert_eq!(cache.full_slabs_list.iter().count(), 1);

            // Alloc 0.5 slab
            let mut second_slab_ptrs = Vec::new();
            for _ in 0..cache.objects_per_slab / 2 {
                second_slab_ptrs.push(cache.alloc());
                assert!(!second_slab_ptrs.last().unwrap().is_null());
                assert!(second_slab_ptrs.last().unwrap().is_aligned());
            }

            // 1 free slabs, 1 full slabs
            assert_eq!(cache.free_slabs_list.iter().count(), 1);
            assert_eq!(cache.full_slabs_list.iter().count(), 1);

            // Free first slab
            first_slab_ptrs.shuffle(&mut rand::thread_rng());
            for v in first_slab_ptrs.iter() {
                cache.free(*v);
            }

            // 1 free slabs, 0 full slabs
            assert_eq!(cache.free_slabs_list.iter().count(), 1);
            assert_eq!(cache.full_slabs_list.iter().count(), 0);

            // Free second slab
            second_slab_ptrs.shuffle(&mut rand::thread_rng());
            for v in second_slab_ptrs.iter() {
                cache.free(*v);
            }

            // All memory free
            // 0 free slabs, 0 full slabs
            assert_eq!(cache.free_slabs_list.iter().count(), 0);
            assert_eq!(cache.full_slabs_list.iter().count(), 0);
            assert_eq!(cache.memory_backend.allocated_slab_addrs.len(), 0);
            assert_eq!(cache.memory_backend.allocated_slab_info_addrs.len(), 0);
            assert!(cache.memory_backend.ht_saved_slab_infos.is_empty());

            // Random test

            // Random number of test
            for _ in 0..rand::thread_rng().gen_range(20..=40) {
                let mut allocated_ptrs = Vec::new();

                for _ in 10..=20 {
                    // Alloc or free
                    if rand::thread_rng().gen_bool(0.5) {
                        // Alloc random number of objects
                        for _ in 0..rand::thread_rng().gen_range(20..100) {
                            let allocated_ptr = cache.alloc();
                            assert!(!allocated_ptr.is_null());
                            assert!(allocated_ptr.is_aligned());
                            allocated_ptrs.push(allocated_ptr);
                            // Fill allocated memory
                            let random_byte: u8 = rand::thread_rng().gen_range(0u8..=255u8);
                            (*allocated_ptr).first_bytes = [random_byte; 128];
                            (*allocated_ptr).ptr_address = allocated_ptr as u64;
                            (*allocated_ptr).last_bytes = [random_byte; 120];
                        }
                    } else {
                        allocated_ptrs.shuffle(&mut rand::thread_rng());
                        // Free random number of objects
                        for _ in 0..rand::thread_rng().gen_range(0..=allocated_ptrs.len()) {
                            let freed_ptr = allocated_ptrs.pop().unwrap();
                            // Check memory
                            assert_eq!(
                                (*freed_ptr).first_bytes,
                                [(*freed_ptr).first_bytes[0]; 128]
                            );
                            assert_eq!((*freed_ptr).ptr_address, freed_ptr as u64);
                            assert_eq!((*freed_ptr).last_bytes, [(*freed_ptr).last_bytes[0]; 120]);
                            cache.free(freed_ptr);
                        }
                    }
                }

                // All addresses are unique
                let hs: HashSet<_> = HashSet::from_iter(allocated_ptrs.clone().into_iter());
                assert_eq!(hs.len(), allocated_ptrs.len());
                assert_eq!(
                    cache.statistics.allocated_objects_number,
                    allocated_ptrs.len()
                );

                // Check statistics
                assert_eq!(
                    cache.statistics.allocated_objects_number,
                    allocated_ptrs.len()
                );
                let mut free_objects_counter = 0;
                for free_slab_info in cache.free_slabs_list.iter() {
                    free_objects_counter += (*free_slab_info.data.lock().get()).free_objects_number;
                }
                assert_eq!(cache.statistics.free_objects_number, free_objects_counter);
                assert_eq!(
                    cache.statistics.full_slabs_number,
                    cache.full_slabs_list.iter().count()
                );
                assert_eq!(
                    cache.statistics.free_slabs_number,
                    cache.free_slabs_list.iter().count()
                );

                // Free all objects
                allocated_ptrs.shuffle(&mut rand::thread_rng());
                for v in allocated_ptrs.into_iter() {
                    cache.free(v);
                }
                assert_eq!(cache.memory_backend.allocated_slab_addrs.len(), 0);
            }
            assert!(cache.free_slabs_list.is_empty());
            assert!(cache.full_slabs_list.is_empty());
            assert_eq!(cache.memory_backend.allocated_slab_addrs.len(), 0);
            assert_eq!(cache.memory_backend.allocated_slab_info_addrs.len(), 0);
            assert!(cache.memory_backend.ht_saved_slab_infos.is_empty());

            // Check statistics
            assert_eq!(cache.statistics.free_slabs_number, 0);
            assert_eq!(cache.statistics.full_slabs_number, 0);
            assert_eq!(cache.statistics.allocated_objects_number, 0);
            assert_eq!(cache.statistics.free_objects_number, 0);
        }
    }

    // Alloc and free
    // Random test
    // Small, slab size == page size
    // No SlabInfo allocation/free
    // No SlabInfo save/get
    #[test]
    fn _08_free_small_ss_eq_ps_single_object() {
        unsafe {
            const PAGE_SIZE: usize = 4096;
            const SLAB_SIZE: usize = 4096;
            const OBJECT_SIZE_TYPE: ObjectSizeType = ObjectSizeType::Small;

            #[repr(C)]
            struct TestObjectType3200 {
                first_bytes: [u8; 2048], // 2048
                ptr_address: u64,        // 8
                last_bytes: [u8; 1144],  // 1144
            }
            assert_eq!(size_of::<TestObjectType3200>(), 3200);

            struct TestMemoryBackend {
                allocated_slab_addrs: Vec<usize>,
            }

            impl MemoryBackend for TestMemoryBackend {
                unsafe fn alloc_slab(&mut self, slab_size: usize, page_size: usize) -> *mut u8 {
                    assert_eq!(slab_size, SLAB_SIZE);
                    assert_eq!(page_size, PAGE_SIZE);
                    let layout = Layout::from_size_align(slab_size, page_size).unwrap();
                    let allocated_slab_ptr = alloc(layout);
                    assert!(!allocated_slab_ptr.is_null());
                    self.allocated_slab_addrs.push(allocated_slab_ptr as usize);
                    allocated_slab_ptr
                }

                unsafe fn free_slab(
                    &mut self,
                    slab_ptr: *mut u8,
                    slab_size: usize,
                    page_size: usize,
                ) {
                    let position = self
                        .allocated_slab_addrs
                        .iter()
                        .position(|addr| *addr == slab_ptr as usize)
                        .unwrap();
                    self.allocated_slab_addrs.remove(position);
                    assert_eq!(slab_size, SLAB_SIZE);
                    assert_eq!(page_size, PAGE_SIZE);
                    let layout = Layout::from_size_align(slab_size, page_size).unwrap();
                    dealloc(slab_ptr, layout);
                }

                unsafe fn alloc_slab_info(&mut self) -> *mut SlabInfo {
                    unreachable!();
                }

                unsafe fn free_slab_info(&mut self, _slab_info_ptr: *mut SlabInfo) {
                    unreachable!();
                }

                unsafe fn save_slab_info_addr(
                    &mut self,
                    _object_page_addr: usize,
                    _slab_info_ptr: *mut SlabInfo,
                ) {
                    unreachable!();
                }

                unsafe fn get_slab_info_addr(&mut self, _object_page_addr: usize) -> *mut SlabInfo {
                    unreachable!();
                }

                unsafe fn delete_slab_info_addr(&mut self, _page_addr: usize) {}
            }

            let test_memory_backend = TestMemoryBackend {
                allocated_slab_addrs: Vec::new(),
            };

            // Create cache
            // 1 object
            let mut cache: Cache<TestObjectType3200, TestMemoryBackend> =
                Cache::new(SLAB_SIZE, PAGE_SIZE, OBJECT_SIZE_TYPE, test_memory_backend).unwrap();
            assert_eq!(cache.objects_per_slab, 1);

            // Alloc 1 object
            let allocated_ptr = cache.alloc();
            assert!(!allocated_ptr.is_null());
            assert!(allocated_ptr.is_aligned());
            assert_eq!(cache.memory_backend.allocated_slab_addrs.len(), 1);
            // Free 1 object
            cache.free(allocated_ptr);
            assert_eq!(cache.memory_backend.allocated_slab_addrs.len(), 0);

            // Random number of test
            for _ in 0..rand::thread_rng().gen_range(20..=40) {
                let mut allocated_ptrs = Vec::new();

                for _ in 10..=20 {
                    // Alloc or free
                    if rand::thread_rng().gen_bool(0.5) {
                        // Alloc random number of objects/slabs
                        for _ in 0..rand::thread_rng().gen_range(20..100) {
                            let allocated_ptr = cache.alloc();
                            assert!(!allocated_ptr.is_null());
                            assert!(allocated_ptr.is_aligned());
                            allocated_ptrs.push(allocated_ptr);
                            // Fill allocated memory
                            let random_byte: u8 = rand::thread_rng().gen_range(0u8..=255u8);
                            (*allocated_ptr).first_bytes = [random_byte; 2048];
                            (*allocated_ptr).ptr_address = allocated_ptr as u64;
                            (*allocated_ptr).last_bytes = [random_byte; 1144];
                        }
                        assert_eq!(
                            allocated_ptrs.len(),
                            cache.memory_backend.allocated_slab_addrs.len()
                        );
                    } else {
                        allocated_ptrs.shuffle(&mut rand::thread_rng());
                        // Free random number of objects/slabs
                        for _ in 0..rand::thread_rng().gen_range(0..=allocated_ptrs.len()) {
                            let freed_ptr = allocated_ptrs.pop().unwrap();
                            // Check memory
                            assert_eq!(
                                (*freed_ptr).first_bytes,
                                [(*freed_ptr).first_bytes[0]; 2048]
                            );
                            assert_eq!((*freed_ptr).ptr_address, freed_ptr as u64);
                            assert_eq!((*freed_ptr).last_bytes, [(*freed_ptr).last_bytes[0]; 1144]);
                            cache.free(freed_ptr);
                        }
                        assert_eq!(
                            allocated_ptrs.len(),
                            cache.memory_backend.allocated_slab_addrs.len()
                        );
                    }
                }

                // All addresses are unique
                let hs: HashSet<_> = HashSet::from_iter(allocated_ptrs.clone().into_iter());
                assert_eq!(hs.len(), allocated_ptrs.len());
                assert_eq!(
                    cache.statistics.allocated_objects_number,
                    allocated_ptrs.len()
                );

                // Check statistics
                assert_eq!(
                    cache.statistics.allocated_objects_number,
                    allocated_ptrs.len()
                );
                let mut free_objects_counter = 0;
                for free_slab_info in cache.free_slabs_list.iter() {
                    free_objects_counter += (*free_slab_info.data.lock().get()).free_objects_number;
                }
                assert_eq!(cache.statistics.free_objects_number, free_objects_counter);
                assert_eq!(
                    cache.statistics.full_slabs_number,
                    cache.full_slabs_list.iter().count()
                );
                assert_eq!(
                    cache.statistics.free_slabs_number,
                    cache.free_slabs_list.iter().count()
                );

                // Free all objects
                allocated_ptrs.shuffle(&mut rand::thread_rng());
                for v in allocated_ptrs.into_iter() {
                    cache.free(v);
                }
                assert_eq!(cache.memory_backend.allocated_slab_addrs.len(), 0);
            }

            assert!(cache.free_slabs_list.is_empty());
            assert!(cache.full_slabs_list.is_empty());
            assert_eq!(cache.memory_backend.allocated_slab_addrs.len(), 0);

            // Check statistics
            assert_eq!(cache.statistics.free_slabs_number, 0);
            assert_eq!(cache.statistics.full_slabs_number, 0);
            assert_eq!(cache.statistics.allocated_objects_number, 0);
            assert_eq!(cache.statistics.free_objects_number, 0);
        }
    }

    // Alloc and free
    // Random test
    // Small, slab size >= page size
    // No SlabInfo allocation/free
    // SlabInfo save/get
    #[test]
    fn _09_free_small_ss_neq_ps_single_object() {
        unsafe {
            const PAGE_SIZE: usize = 4096;
            const SLAB_SIZE: usize = 8192;
            const OBJECT_SIZE_TYPE: ObjectSizeType = ObjectSizeType::Small;

            #[repr(C)]
            struct TestObjectType6400 {
                first_bytes: [u8; 2048], // 2048
                ptr_address: u64,        // 8
                last_bytes: [u8; 4344],  // 4344
            }
            assert_eq!(size_of::<TestObjectType6400>(), 6400);

            struct TestMemoryBackend {
                allocated_slab_addrs: Vec<usize>,
                ht_saved_slab_infos: HashMap<usize, *mut SlabInfo>,
            }

            impl MemoryBackend for TestMemoryBackend {
                unsafe fn alloc_slab(&mut self, slab_size: usize, page_size: usize) -> *mut u8 {
                    assert_eq!(slab_size, SLAB_SIZE);
                    assert_eq!(page_size, PAGE_SIZE);
                    let layout = Layout::from_size_align(slab_size, page_size).unwrap();
                    let allocated_slab_ptr = alloc(layout);
                    assert!(!allocated_slab_ptr.is_null());
                    self.allocated_slab_addrs.push(allocated_slab_ptr as usize);
                    allocated_slab_ptr
                }

                unsafe fn free_slab(
                    &mut self,
                    slab_ptr: *mut u8,
                    slab_size: usize,
                    page_size: usize,
                ) {
                    let position = self
                        .allocated_slab_addrs
                        .iter()
                        .position(|addr| *addr == slab_ptr as usize)
                        .unwrap();
                    self.allocated_slab_addrs.remove(position);
                    assert_eq!(slab_size, SLAB_SIZE);
                    assert_eq!(page_size, PAGE_SIZE);
                    let layout = Layout::from_size_align(slab_size, page_size).unwrap();
                    dealloc(slab_ptr, layout);
                }

                unsafe fn alloc_slab_info(&mut self) -> *mut SlabInfo {
                    unreachable!();
                }

                unsafe fn free_slab_info(&mut self, _slab_info_ptr: *mut SlabInfo) {
                    unreachable!();
                }

                unsafe fn save_slab_info_addr(
                    &mut self,
                    object_page_addr: usize,
                    slab_info_ptr: *mut SlabInfo,
                ) {
                    self.ht_saved_slab_infos
                        .insert(object_page_addr, slab_info_ptr);
                }

                unsafe fn get_slab_info_addr(&mut self, object_page_addr: usize) -> *mut SlabInfo {
                    self.ht_saved_slab_infos
                        .get(&object_page_addr)
                        .unwrap()
                        .cast()
                }

                unsafe fn delete_slab_info_addr(&mut self, page_addr: usize) {
                    self.ht_saved_slab_infos.remove(&page_addr);
                }
            }

            let test_memory_backend = TestMemoryBackend {
                allocated_slab_addrs: Vec::new(),
                ht_saved_slab_infos: HashMap::new(),
            };

            // Create cache
            // 1 object
            let mut cache: Cache<TestObjectType6400, TestMemoryBackend> =
                Cache::new(SLAB_SIZE, PAGE_SIZE, OBJECT_SIZE_TYPE, test_memory_backend).unwrap();
            assert_eq!(cache.objects_per_slab, 1);

            // Alloc 1 object
            let allocated_ptr = cache.alloc();
            assert!(!allocated_ptr.is_null());
            assert!(allocated_ptr.is_aligned());
            assert_eq!(cache.memory_backend.allocated_slab_addrs.len(), 1);
            assert_eq!(cache.memory_backend.ht_saved_slab_infos.len(), 1);
            // Free 1 object
            cache.free(allocated_ptr);
            assert_eq!(cache.memory_backend.allocated_slab_addrs.len(), 0);
            assert!(cache.memory_backend.ht_saved_slab_infos.is_empty());

            // Random number of test
            for _ in 0..rand::thread_rng().gen_range(20..=40) {
                let mut allocated_ptrs = Vec::new();

                for _ in 10..=20 {
                    // Alloc or free
                    if rand::thread_rng().gen_bool(0.5) {
                        // Alloc random number of objects/slabs
                        for _ in 0..rand::thread_rng().gen_range(20..100) {
                            let allocated_ptr = cache.alloc();
                            assert!(!allocated_ptr.is_null());
                            assert!(allocated_ptr.is_aligned());
                            allocated_ptrs.push(allocated_ptr);
                            // Fill allocated memory
                            let random_byte: u8 = rand::thread_rng().gen_range(0u8..=255u8);
                            (*allocated_ptr).first_bytes = [random_byte; 2048];
                            (*allocated_ptr).ptr_address = allocated_ptr as u64;
                            (*allocated_ptr).last_bytes = [random_byte; 4344];
                        }
                        assert_eq!(
                            allocated_ptrs.len(),
                            cache.memory_backend.allocated_slab_addrs.len()
                        );
                    } else {
                        allocated_ptrs.shuffle(&mut rand::thread_rng());
                        // Free random number of objects/slabs
                        for _ in 0..rand::thread_rng().gen_range(0..=allocated_ptrs.len()) {
                            let freed_ptr = allocated_ptrs.pop().unwrap();
                            // Check memory
                            assert_eq!(
                                (*freed_ptr).first_bytes,
                                [(*freed_ptr).first_bytes[0]; 2048]
                            );
                            assert_eq!((*freed_ptr).ptr_address, freed_ptr as u64);
                            assert_eq!((*freed_ptr).last_bytes, [(*freed_ptr).last_bytes[0]; 4344]);
                            cache.free(freed_ptr);
                        }
                        assert_eq!(
                            allocated_ptrs.len(),
                            cache.memory_backend.allocated_slab_addrs.len()
                        );
                    }
                }

                // All addresses are unique
                let hs: HashSet<_> = HashSet::from_iter(allocated_ptrs.clone().into_iter());
                assert_eq!(hs.len(), allocated_ptrs.len());
                assert_eq!(
                    cache.statistics.allocated_objects_number,
                    allocated_ptrs.len()
                );

                // Check statistics
                assert_eq!(
                    cache.statistics.allocated_objects_number,
                    allocated_ptrs.len()
                );
                let mut free_objects_counter = 0;
                for free_slab_info in cache.free_slabs_list.iter() {
                    free_objects_counter += (*free_slab_info.data.lock().get()).free_objects_number;
                }
                assert_eq!(cache.statistics.free_objects_number, free_objects_counter);
                assert_eq!(
                    cache.statistics.full_slabs_number,
                    cache.full_slabs_list.iter().count()
                );
                assert_eq!(
                    cache.statistics.free_slabs_number,
                    cache.free_slabs_list.iter().count()
                );

                // Free all objects
                allocated_ptrs.shuffle(&mut rand::thread_rng());
                for v in allocated_ptrs.into_iter() {
                    cache.free(v);
                }
                assert_eq!(cache.memory_backend.allocated_slab_addrs.len(), 0);
                assert!(cache.memory_backend.ht_saved_slab_infos.is_empty());
            }

            assert!(cache.free_slabs_list.is_empty());
            assert!(cache.full_slabs_list.is_empty());
            assert_eq!(cache.memory_backend.allocated_slab_addrs.len(), 0);
            assert!(cache.memory_backend.ht_saved_slab_infos.is_empty());

            // Check statistics
            assert_eq!(cache.statistics.free_slabs_number, 0);
            assert_eq!(cache.statistics.full_slabs_number, 0);
            assert_eq!(cache.statistics.allocated_objects_number, 0);
            assert_eq!(cache.statistics.free_objects_number, 0);
        }
    }

    // Alloc and free
    // Random test
    // Large, slab size == page size
    // SlabInfo allocation/free
    // SlabInfo save/get
    #[test]
    fn _10_free_large_ss_eq_ps_single_object() {
        unsafe {
            const PAGE_SIZE: usize = 4096;
            const SLAB_SIZE: usize = 4096;
            const OBJECT_SIZE_TYPE: ObjectSizeType = ObjectSizeType::Large;

            #[repr(C)]
            struct TestObjectType3200 {
                first_bytes: [u8; 1144], // 1144
                ptr_address: u64,        // 8
                last_bytes: [u8; 2048],  // 2048
            }
            assert_eq!(size_of::<TestObjectType3200>(), 3200);

            struct TestMemoryBackend {
                allocated_slab_addrs: Vec<usize>,
                allocated_slab_info_addrs: Vec<usize>,
                ht_saved_slab_infos: HashMap<usize, *mut SlabInfo>,
            }

            impl MemoryBackend for TestMemoryBackend {
                unsafe fn alloc_slab(&mut self, slab_size: usize, page_size: usize) -> *mut u8 {
                    assert_eq!(slab_size, SLAB_SIZE);
                    assert_eq!(page_size, PAGE_SIZE);
                    let layout = Layout::from_size_align(slab_size, page_size).unwrap();
                    let allocated_slab_ptr = alloc(layout);
                    assert!(!allocated_slab_ptr.is_null());
                    self.allocated_slab_addrs.push(allocated_slab_ptr as usize);
                    allocated_slab_ptr
                }

                unsafe fn free_slab(
                    &mut self,
                    slab_ptr: *mut u8,
                    slab_size: usize,
                    page_size: usize,
                ) {
                    let position = self
                        .allocated_slab_addrs
                        .iter()
                        .position(|addr| *addr == slab_ptr as usize)
                        .unwrap();
                    self.allocated_slab_addrs.remove(position);
                    assert_eq!(slab_size, SLAB_SIZE);
                    assert_eq!(page_size, PAGE_SIZE);
                    let layout = Layout::from_size_align(slab_size, page_size).unwrap();
                    dealloc(slab_ptr, layout);
                }

                unsafe fn alloc_slab_info(&mut self) -> *mut SlabInfo {
                    let layout = Layout::new::<SlabInfo>();
                    let allocated_ptr = alloc(layout);
                    assert!(!allocated_ptr.is_null());
                    self.allocated_slab_info_addrs.push(allocated_ptr as usize);
                    allocated_ptr.cast()
                }

                unsafe fn free_slab_info(&mut self, slab_info_ptr: *mut SlabInfo) {
                    assert!(!slab_info_ptr.is_null());
                    let layout = Layout::new::<SlabInfo>();
                    let position = self
                        .allocated_slab_info_addrs
                        .iter()
                        .position(|v| *v == slab_info_ptr as usize)
                        .unwrap();
                    self.allocated_slab_info_addrs.remove(position);
                    dealloc(slab_info_ptr.cast(), layout);
                }

                unsafe fn save_slab_info_addr(
                    &mut self,
                    object_page_addr: usize,
                    slab_info_ptr: *mut SlabInfo,
                ) {
                    self.ht_saved_slab_infos
                        .insert(object_page_addr, slab_info_ptr);
                }

                unsafe fn get_slab_info_addr(&mut self, object_page_addr: usize) -> *mut SlabInfo {
                    self.ht_saved_slab_infos
                        .get(&object_page_addr)
                        .unwrap()
                        .cast()
                }

                unsafe fn delete_slab_info_addr(&mut self, page_addr: usize) {
                    assert!(self.ht_saved_slab_infos.remove(&page_addr).is_some());
                }
            }

            let test_memory_backend = TestMemoryBackend {
                allocated_slab_addrs: Vec::new(),
                ht_saved_slab_infos: HashMap::new(),
                allocated_slab_info_addrs: Vec::new(),
            };

            // Create cache
            // 1 object
            let mut cache: Cache<TestObjectType3200, TestMemoryBackend> =
                Cache::new(SLAB_SIZE, PAGE_SIZE, OBJECT_SIZE_TYPE, test_memory_backend).unwrap();
            assert_eq!(cache.objects_per_slab, 1);

            // Alloc 1 object
            let allocated_ptr = cache.alloc();
            assert!(!allocated_ptr.is_null());
            assert!(allocated_ptr.is_aligned());
            assert_eq!(cache.memory_backend.allocated_slab_addrs.len(), 1);
            assert_eq!(cache.memory_backend.ht_saved_slab_infos.len(), 1);
            // Free 1 object
            cache.free(allocated_ptr);
            assert_eq!(cache.memory_backend.allocated_slab_addrs.len(), 0);
            assert!(cache.memory_backend.ht_saved_slab_infos.is_empty());

            // Random number of test
            for _ in 0..rand::thread_rng().gen_range(20..=40) {
                let mut allocated_ptrs = Vec::new();

                for _ in 10..=20 {
                    // Alloc or free
                    if rand::thread_rng().gen_bool(0.5) {
                        // Alloc random number of objects/slabs
                        for _ in 0..rand::thread_rng().gen_range(20..100) {
                            let allocated_ptr = cache.alloc();
                            assert!(!allocated_ptr.is_null());
                            assert!(allocated_ptr.is_aligned());
                            allocated_ptrs.push(allocated_ptr);
                            // Fill allocated memory
                            let random_byte: u8 = rand::thread_rng().gen_range(0u8..=255u8);
                            (*allocated_ptr).first_bytes = [random_byte; 1144];
                            (*allocated_ptr).ptr_address = allocated_ptr as u64;
                            (*allocated_ptr).last_bytes = [random_byte; 2048];
                        }
                        assert_eq!(
                            allocated_ptrs.len(),
                            cache.memory_backend.allocated_slab_addrs.len()
                        );
                    } else {
                        allocated_ptrs.shuffle(&mut rand::thread_rng());
                        // Free random number of objects/slabs
                        for _ in 0..rand::thread_rng().gen_range(0..=allocated_ptrs.len()) {
                            let freed_ptr = allocated_ptrs.pop().unwrap();
                            // Check memory
                            assert_eq!(
                                (*freed_ptr).first_bytes,
                                [(*freed_ptr).first_bytes[0]; 1144]
                            );
                            assert_eq!((*freed_ptr).ptr_address, freed_ptr as u64);
                            assert_eq!((*freed_ptr).last_bytes, [(*freed_ptr).last_bytes[0]; 2048]);
                            cache.free(freed_ptr);
                        }
                        assert_eq!(
                            allocated_ptrs.len(),
                            cache.memory_backend.allocated_slab_addrs.len()
                        );
                    }
                }

                // All addresses are unique
                let hs: HashSet<_> = HashSet::from_iter(allocated_ptrs.clone().into_iter());
                assert_eq!(hs.len(), allocated_ptrs.len());
                assert_eq!(
                    cache.statistics.allocated_objects_number,
                    allocated_ptrs.len()
                );

                // Check statistics
                assert_eq!(
                    cache.statistics.allocated_objects_number,
                    allocated_ptrs.len()
                );
                let mut free_objects_counter = 0;
                for free_slab_info in cache.free_slabs_list.iter() {
                    free_objects_counter += (*free_slab_info.data.lock().get()).free_objects_number;
                }
                assert_eq!(cache.statistics.free_objects_number, free_objects_counter);
                assert_eq!(
                    cache.statistics.full_slabs_number,
                    cache.full_slabs_list.iter().count()
                );
                assert_eq!(
                    cache.statistics.free_slabs_number,
                    cache.free_slabs_list.iter().count()
                );

                // Free all objects
                allocated_ptrs.shuffle(&mut rand::thread_rng());
                for v in allocated_ptrs.into_iter() {
                    cache.free(v);
                }
                assert_eq!(cache.memory_backend.allocated_slab_addrs.len(), 0);
                assert!(cache.memory_backend.ht_saved_slab_infos.is_empty());
            }

            assert!(cache.free_slabs_list.is_empty());
            assert!(cache.full_slabs_list.is_empty());
            assert_eq!(cache.memory_backend.allocated_slab_addrs.len(), 0);
            assert_eq!(cache.memory_backend.allocated_slab_info_addrs.len(), 0);
            assert!(cache.memory_backend.ht_saved_slab_infos.is_empty());

            // Check statistics
            assert_eq!(cache.statistics.free_slabs_number, 0);
            assert_eq!(cache.statistics.full_slabs_number, 0);
            assert_eq!(cache.statistics.allocated_objects_number, 0);
            assert_eq!(cache.statistics.free_objects_number, 0);
        }
    }

    // Alloc and free
    // Random test
    // Large, slab size >= page size
    // SlabInfo allocation/free
    // SlabInfo save/get
    #[test]
    fn _11_free_large_ss_neq_ps_single_object() {
        unsafe {
            const PAGE_SIZE: usize = 4096;
            const SLAB_SIZE: usize = 8192;
            const OBJECT_SIZE_TYPE: ObjectSizeType = ObjectSizeType::Large;

            #[repr(C)]
            struct TestObjectType6400 {
                first_bytes: [u8; 4344], // 4344
                ptr_address: u64,        // 8
                last_bytes: [u8; 2048],  // 2048
            }
            assert_eq!(size_of::<TestObjectType6400>(), 6400);

            struct TestMemoryBackend {
                allocated_slab_addrs: Vec<usize>,
                allocated_slab_info_addrs: Vec<usize>,
                ht_saved_slab_infos: HashMap<usize, *mut SlabInfo>,
            }

            impl MemoryBackend for TestMemoryBackend {
                unsafe fn alloc_slab(&mut self, slab_size: usize, page_size: usize) -> *mut u8 {
                    assert_eq!(slab_size, SLAB_SIZE);
                    assert_eq!(page_size, PAGE_SIZE);
                    let layout = Layout::from_size_align(slab_size, page_size).unwrap();
                    let allocated_slab_ptr = alloc(layout);
                    assert!(!allocated_slab_ptr.is_null());
                    self.allocated_slab_addrs.push(allocated_slab_ptr as usize);
                    allocated_slab_ptr
                }

                unsafe fn free_slab(
                    &mut self,
                    slab_ptr: *mut u8,
                    slab_size: usize,
                    page_size: usize,
                ) {
                    let position = self
                        .allocated_slab_addrs
                        .iter()
                        .position(|addr| *addr == slab_ptr as usize)
                        .unwrap();
                    self.allocated_slab_addrs.remove(position);
                    assert_eq!(slab_size, SLAB_SIZE);
                    assert_eq!(page_size, PAGE_SIZE);
                    let layout = Layout::from_size_align(slab_size, page_size).unwrap();
                    dealloc(slab_ptr, layout);
                }

                unsafe fn alloc_slab_info(&mut self) -> *mut SlabInfo {
                    let layout = Layout::new::<SlabInfo>();
                    let allocated_ptr = alloc(layout);
                    assert!(!allocated_ptr.is_null());
                    self.allocated_slab_info_addrs.push(allocated_ptr as usize);
                    allocated_ptr.cast()
                }

                unsafe fn free_slab_info(&mut self, slab_info_ptr: *mut SlabInfo) {
                    assert!(!slab_info_ptr.is_null());
                    let layout = Layout::new::<SlabInfo>();
                    let position = self
                        .allocated_slab_info_addrs
                        .iter()
                        .position(|v| *v == slab_info_ptr as usize)
                        .unwrap();
                    self.allocated_slab_info_addrs.remove(position);
                    dealloc(slab_info_ptr.cast(), layout);
                }

                unsafe fn save_slab_info_addr(
                    &mut self,
                    object_page_addr: usize,
                    slab_info_ptr: *mut SlabInfo,
                ) {
                    self.ht_saved_slab_infos
                        .insert(object_page_addr, slab_info_ptr);
                }

                unsafe fn get_slab_info_addr(&mut self, object_page_addr: usize) -> *mut SlabInfo {
                    self.ht_saved_slab_infos
                        .get(&object_page_addr)
                        .unwrap()
                        .cast()
                }

                unsafe fn delete_slab_info_addr(&mut self, page_addr: usize) {
                    self.ht_saved_slab_infos.remove(&page_addr);
                }
            }

            let test_memory_backend = TestMemoryBackend {
                allocated_slab_addrs: Vec::new(),
                ht_saved_slab_infos: HashMap::new(),
                allocated_slab_info_addrs: Vec::new(),
            };

            // Create cache
            // 1 object
            let mut cache: Cache<TestObjectType6400, TestMemoryBackend> =
                Cache::new(SLAB_SIZE, PAGE_SIZE, OBJECT_SIZE_TYPE, test_memory_backend).unwrap();
            assert_eq!(cache.objects_per_slab, 1);

            // Alloc 1 object
            let allocated_ptr = cache.alloc();
            assert!(!allocated_ptr.is_null());
            assert!(allocated_ptr.is_aligned());
            assert_eq!(cache.memory_backend.allocated_slab_addrs.len(), 1);
            assert_eq!(cache.memory_backend.ht_saved_slab_infos.len(), 1);
            // Free 1 object
            cache.free(allocated_ptr);
            assert_eq!(cache.memory_backend.allocated_slab_addrs.len(), 0);

            // Random number of test
            for _ in 0..rand::thread_rng().gen_range(20..=40) {
                let mut allocated_ptrs = Vec::new();

                for _ in 10..=20 {
                    // Alloc or free
                    if rand::thread_rng().gen_bool(0.5) {
                        // Alloc random number of objects/slabs
                        for _ in 0..rand::thread_rng().gen_range(20..100) {
                            let allocated_ptr = cache.alloc();
                            assert!(!allocated_ptr.is_null());
                            assert!(allocated_ptr.is_aligned());
                            allocated_ptrs.push(allocated_ptr);
                            // Fill allocated memory
                            let random_byte: u8 = rand::thread_rng().gen_range(0u8..=255u8);
                            (*allocated_ptr).first_bytes = [random_byte; 4344];
                            (*allocated_ptr).ptr_address = allocated_ptr as u64;
                            (*allocated_ptr).last_bytes = [random_byte; 2048];
                        }
                        assert_eq!(
                            allocated_ptrs.len(),
                            cache.memory_backend.allocated_slab_addrs.len()
                        );
                    } else {
                        allocated_ptrs.shuffle(&mut rand::thread_rng());
                        // Free random number of objects/slabs
                        for _ in 0..rand::thread_rng().gen_range(0..=allocated_ptrs.len()) {
                            let freed_ptr = allocated_ptrs.pop().unwrap();
                            // Check memory
                            assert_eq!(
                                (*freed_ptr).first_bytes,
                                [(*freed_ptr).first_bytes[0]; 4344]
                            );
                            assert_eq!((*freed_ptr).ptr_address, freed_ptr as u64);
                            assert_eq!((*freed_ptr).last_bytes, [(*freed_ptr).last_bytes[0]; 2048]);
                            cache.free(freed_ptr);
                        }
                        assert_eq!(
                            allocated_ptrs.len(),
                            cache.memory_backend.allocated_slab_addrs.len()
                        );
                    }
                }

                // All addresses are unique
                let hs: HashSet<_> = HashSet::from_iter(allocated_ptrs.clone().into_iter());
                assert_eq!(hs.len(), allocated_ptrs.len());
                assert_eq!(
                    cache.statistics.allocated_objects_number,
                    allocated_ptrs.len()
                );

                // Check statistics
                assert_eq!(
                    cache.statistics.allocated_objects_number,
                    allocated_ptrs.len()
                );
                let mut free_objects_counter = 0;
                for free_slab_info in cache.free_slabs_list.iter() {
                    free_objects_counter += (*free_slab_info.data.lock().get()).free_objects_number;
                }
                assert_eq!(cache.statistics.free_objects_number, free_objects_counter);
                assert_eq!(
                    cache.statistics.full_slabs_number,
                    cache.full_slabs_list.iter().count()
                );
                assert_eq!(
                    cache.statistics.free_slabs_number,
                    cache.free_slabs_list.iter().count()
                );

                // Free all objects
                allocated_ptrs.shuffle(&mut rand::thread_rng());
                for v in allocated_ptrs.into_iter() {
                    cache.free(v);
                }
                assert_eq!(cache.memory_backend.allocated_slab_addrs.len(), 0);
                assert!(cache.memory_backend.ht_saved_slab_infos.is_empty());
            }
            assert!(cache.free_slabs_list.is_empty());
            assert!(cache.full_slabs_list.is_empty());
            assert_eq!(cache.memory_backend.allocated_slab_addrs.len(), 0);
            assert_eq!(cache.memory_backend.allocated_slab_info_addrs.len(), 0);
            assert!(cache.memory_backend.ht_saved_slab_infos.is_empty());

            // Check statistics
            assert_eq!(cache.statistics.free_slabs_number, 0);
            assert_eq!(cache.statistics.full_slabs_number, 0);
            assert_eq!(cache.statistics.allocated_objects_number, 0);
            assert_eq!(cache.statistics.free_objects_number, 0);
        }
    }
}
