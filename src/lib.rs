//#![no_std]
#![allow(unused)]

use core::cell::UnsafeCell;
use core::ptr::null_mut;
use intrusive_collections::{intrusive_adapter, LinkedList, LinkedListLink};
use std::cmp::PartialEq;

/// Slab cache for my OS
///
/// Aimed for use with buddy allocator

/// Slab cache
///
/// Stores objects of the type T
struct Cache<'a, T> {
    object_size: usize,
    slab_size: usize,
    page_size: usize,
    object_size_type: ObjectSizeType,
    /// Total objects in slab
    objects_per_slab: usize,
    /// List of slabs with free objects
    free_slabs_list: LinkedList<SlabInfoAdapter<'a, T>>,
    /// List of full slabs
    full_slabs_list: LinkedList<SlabInfoAdapter<'a, T>>,
    memory_backend: &'a mut dyn MemoryBackend<'a, T>,
}

impl<'a, T> Cache<'a, T> {
    /// slab_size must be >= page_size and must be the sum of page_size. I.e. the start and end of slab must be page-aligned.
    ///
    /// memory_backend
    ///
    /// size of T must be >= 8/16 (two pointers)
    pub fn new(
        slab_size: usize,
        page_size: usize,
        object_size_type: ObjectSizeType,
        memory_backend: &'a mut dyn MemoryBackend<'a, T>,
    ) -> Result<Self, &'static str> {
        let object_size = size_of::<T>();
        if object_size < size_of::<FreeObject>() {
            return Err("Object size smaller than 8/16 (two pointers)");
        };
        if let ObjectSizeType::Small = object_size_type {
            if slab_size < size_of::<SlabInfo<T>>() + object_size {
                return Err("Slab size is too small");
            }
        }
        if slab_size % page_size != 0 {
            return Err("The slab_size is not exactly within the page boundaries. Slab must consist of pages.");
        }

        // Calculate number of objects in slab
        let objects_per_slab = match object_size_type {
            ObjectSizeType::Small => {
                let fake_slab_addr = 0usize;
                let fake_slab_info_addr = calculate_slab_info_addr_in_small_object_cache::<T>(
                    fake_slab_addr as *mut u8,
                    slab_size,
                );
                assert!(fake_slab_info_addr > fake_slab_addr);
                assert!(
                    fake_slab_info_addr <= fake_slab_addr + slab_size - size_of::<SlabInfo<T>>()
                );
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
        })
    }

    /// Allocs object from cache
    pub fn alloc(&mut self) -> *mut T {
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
                    let slab_info_addr = calculate_slab_info_addr_in_small_object_cache::<T>(
                        slab_ptr,
                        self.slab_size,
                    );
                    debug_assert!(slab_info_addr > slab_ptr as usize);
                    debug_assert!(
                        slab_info_addr
                            <= slab_ptr as usize + self.slab_size - size_of::<SlabInfo<T>>()
                    );

                    slab_info_addr as *mut SlabInfo<T>
                }
                ObjectSizeType::Large => {
                    // Allocate memory using memory backend
                    self.memory_backend.alloc_slab_info()
                }
            };
            if slab_info_ptr.is_null() {
                // Failed to allocate SlabInfo
                self.memory_backend
                    .free_slab(slab_ptr, self.slab_size, self.page_size);
                return null_mut();
            }
            assert_eq!(
                slab_info_ptr as usize % align_of::<SlabInfo<T>>(),
                0,
                "SlabInfo addr not aligned!"
            );

            // Fill SlabInfo
            unsafe {
                slab_info_ptr.write(SlabInfo {
                    slab_link: LinkedListLink::new(),
                    data: UnsafeCell::new(SlabInfoData {
                        free_objects_list: LinkedList::new(FreeObjectAdapter::new()),
                        cache_ptr: self as *mut Self,
                        free_objects_number: self.objects_per_slab,
                    }),
                });
            };

            // Make SlabInfo ref
            let slab_info_ref = unsafe { &mut *slab_info_ptr };
            let slab_info_data_ref = unsafe { &mut *slab_info_ref.data.get() };
            // Add SlabInfo to free list
            self.free_slabs_list.push_back(slab_info_ref);

            // Fill FreeObjects list
            for free_object_index in 0..self.objects_per_slab {
                // Free object stored in slab
                let free_object_addr = slab_ptr as usize + (free_object_index * self.object_size);
                debug_assert_eq!(
                    free_object_addr % align_of::<FreeObject>(),
                    0,
                    "FreeObject addr not aligned!"
                );
                let free_object_ptr = free_object_addr as *mut FreeObject;
                unsafe {
                    free_object_ptr.write(FreeObject {
                        free_object_link: LinkedListLink::new(),
                    });
                }
                let free_object_ref = unsafe { &*free_object_ptr };

                // Add free object to free objects list
                slab_info_data_ref
                    .free_objects_list
                    .push_back(free_object_ref);
            }
        }
        // Allocate object
        let mut free_object_ptr: *mut T = null_mut();

        // Get free slab info
        let free_slab_info = self.free_slabs_list.back().get().unwrap();
        // Get slab data
        let free_slab_info_data = unsafe { &mut *free_slab_info.data.get() };

        // Get object from FreeObject list
        let free_object_ref = free_slab_info_data.free_objects_list.pop_back().unwrap();
        free_slab_info_data.free_objects_number -= 1;
        free_object_ptr = free_object_ref as *const _ as *mut T;

        // Save SlabInfo ptr
        if !(self.object_size_type == ObjectSizeType::Small && self.slab_size == self.page_size) {
            let free_slab_info_ptr = free_slab_info as *const _;
            let free_object_page_addr = free_object_ptr as usize & !(self.page_size - 1);
            debug_assert_eq!(free_object_page_addr % self.page_size, 0);
            // See this function for more info
            self.memory_backend
                .save_slab_info_ptr(free_slab_info_ptr, free_object_page_addr);
        }

        // Check free objects list
        if free_slab_info_data.free_objects_list.is_empty() {
            // Slab is empty now
            // Remove from free list
            let free_slab_info = self.free_slabs_list.pop_back().unwrap();
            // Add to full list
            self.full_slabs_list.push_back(free_slab_info);
        }

        free_object_ptr
    }

    /// Returns object to cache
    pub fn free(&mut self, object_ptr: *mut T) {
        unimplemented!();
    }
}

fn calculate_slab_info_addr_in_small_object_cache<T>(slab_ptr: *mut u8, slab_size: usize) -> usize {
    // SlabInfo inside slab, at end
    let slab_end_addr = slab_ptr as usize + slab_size;
    (slab_end_addr - size_of::<SlabInfo<T>>()) & !(align_of::<SlabInfo<T>>() - 1)
}

#[derive(Debug, Copy, Clone, PartialEq)]
/// See [ObjectSizeType::Small] and [ObjectSizeType::Large]
enum ObjectSizeType {
    /// For small size objects, SlabInfo is stored directly in slab and little memory is lost.
    /// For example:
    /// slab size: 4096
    /// object size: 32
    /// slab info: 40
    /// We will be able to place 126 objects, this will consume 4032 bytes, the 40 bytes will be occupied by SlabInfo, only 24 bytes will be lost, all is well.
    Small,
    /// For large size objects, SlabInfo can't be stored directly in slab and allocates using MemoryBackend.
    /// For example:
    /// slab size: 4096
    /// object size: 2048
    /// slab info: 40
    /// We will be able to place only 1 objects, this will consume 2048 bytes, the 40 bytes will be occupied by SlabInfo, 2008 bytes will be lost!
    Large,
}

#[repr(C)]
/// Slab info
///
/// Stored in slab(for small objects slab) or allocatated from another slab(for large objects slab)
struct SlabInfo<'a, T> {
    /// Link to next and prev slab
    slab_link: LinkedListLink,
    /// LinkedList doesn't give mutable access to data, we have to snip the data in UnsafeCell
    data: UnsafeCell<SlabInfoData<'a, T>>,
}

struct SlabInfoData<'a, T> {
    /// Free objects in slab list
    free_objects_list: LinkedList<FreeObjectAdapter<'a>>,
    /// Slab cache to which slab belongs
    cache_ptr: *mut Cache<'a, T>,
    /// Number of free objects in slab
    free_objects_number: usize,
}

#[derive(Debug)]
#[repr(transparent)]
/// Metadata stored inside a free object and pointing to the previous and next free object
struct FreeObject {
    free_object_link: LinkedListLink,
}

intrusive_adapter!(SlabInfoAdapter<'a, T> = &'a SlabInfo<'a, T>: SlabInfo<T> { slab_link: LinkedListLink });
intrusive_adapter!(FreeObjectAdapter<'a> = &'a FreeObject: FreeObject { free_object_link: LinkedListLink });

/// Used by slab cache for allocating slabs and SlabInfo's
///
/// Slab caching logic can be placed here
///
/// alloc_slab_info() and free_slab_info() not used by small objects cache and can always return null
trait MemoryBackend<'a, T> {
    /// Allocates slab for cache
    ///
    /// Must be page aligned
    fn alloc_slab(&mut self, slab_size: usize, page_size: usize) -> *mut u8;

    /// Frees slab
    fn free_slab(&mut self, slab_ptr: *mut u8, slab_size: usize, page_size: usize);

    /// Allocs SlabInfo
    ///
    /// Not used by small object cache and can always return null
    fn alloc_slab_info(&mut self) -> *mut SlabInfo<'a, T>;

    /// Frees SlabInfo
    ///
    /// Not used by small object cache and can always return null
    fn free_slab_info(&mut self, slab_ptr: *mut SlabInfo<'a, T>);

    /// It is required to save slab_info_ptr to the corresponding ***down page aligned*** object_ptr (page addr)
    ///
    /// This function cannot be called just for the cache which: [ObjectSizeType::Small] and slab_size == page_size.
    /// In this case the allocator is able to calculate its address itself.
    ///
    /// How it works: when an object is returned to the cache, SlabInfo needs to be found.
    /// SlabInfo can be stored in the slab itself([ObjectSizeType::Small]) or outside of it([ObjectSizeType::Large]).
    /// In the case of Large it is impossible to calculate this address exactly, in the case of Small it is impossible to calculate this address because the start of the slab is unknown.
    /// Only when slab_size == page_size, we know for sure that the slab starts at the beginning of the page.
    ///
    ///
    /// # IMPORTANT
    /// * Since the beginning of slab is aligned to the beginning of a page, and slab exactly in pages you can save slab_info_ptr to the page address of the page to which the object belongs.
    /// Hash table good for this
    /// ```ignore
    /// // key value
    /// hashtable.insert(object_page_addr, slab_info_ptr);
    /// ```
    ///
    ///  |   SLAB0   | <-- 1 slabs, 1 slab info
    ///  |o0;o1|o2;o3| <-- 2 pages (2 pages in slab)
    /// If you align the address of the object to the page, you can unambiguously refer it to the correct slab (slab page) and calculate SlabInfo by the slab page as well.
    /// Not only is it incredibly wasteful to save SlabInfo for each object, but it doesn't make sense. But this trick works only when the beginning of the slab is aligned to the beginning of the page and when its size is the sum of page sizes.
    fn save_slab_info_ptr(
        &mut self,
        slab_info_ptr: *const SlabInfo<'a, T>,
        object_page_addr: usize,
    );

    /// It is required to get slab_info_ptr to the corresponding ***down page aligned*** object_ptr (page addr)
    fn get_slab_info_ptr(&mut self, slab_info_ptr: *const SlabInfo<'a, T>, object_page_addr: usize);
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;
    extern crate alloc;
    extern crate std;
    use alloc::alloc::{alloc, dealloc, Layout};
    use alloc::vec::Vec;

    #[test]
    fn alloc_only_small() {
        const SLAB_SIZE: usize = 4096;
        const PAGE_SIZE: usize = 4096;
        struct TestMemoryBackend {
            allocated_slabs_addrs: Vec<usize>,
        }

        impl<'a, T> MemoryBackend<'a, T> for TestMemoryBackend {
            fn alloc_slab(&mut self, slab_size: usize, page_size: usize) -> *mut u8 {
                assert_eq!(slab_size, SLAB_SIZE);
                assert_eq!(page_size, PAGE_SIZE);
                let layout = Layout::from_size_align(slab_size, page_size).unwrap();
                let allocated_slab_ptr = unsafe { alloc(layout) };
                let allocated_slab_addr = allocated_slab_ptr as usize;
                self.allocated_slabs_addrs.push(allocated_slab_addr);
                unsafe { allocated_slab_ptr }
            }

            fn free_slab(&mut self, slab_ptr: *mut u8, slab_size: usize, page_size: usize) {
                unreachable!();
            }

            fn alloc_slab_info(&mut self) -> *mut SlabInfo<'a, T> {
                unreachable!();
            }

            fn free_slab_info(&mut self, slab_ptr: *mut SlabInfo<'a, T>) {
                unreachable!();
            }

            fn save_slab_info_ptr(
                &mut self,
                slab_info_ptr: *const SlabInfo<'a, T>,
                object_page_addr: usize,
            ) {
                assert_eq!(object_page_addr % PAGE_SIZE, 0);
                // Don't need in this test
            }

            fn get_slab_info_ptr(
                &mut self,
                slab_info_ptr: *const SlabInfo<'a, T>,
                object_page_addr: usize,
            ) {
                // Don't need in this test
                unreachable!();
            }
        }

        let mut test_memory_backend = TestMemoryBackend {
            allocated_slabs_addrs: Vec::new(),
        };
        let test_memory_backend_ptr = &raw mut test_memory_backend;

        struct TestObjectType {
            data: [u8; 1024],
        }
        assert_eq!(size_of::<TestObjectType>(), 1024);

        // 3 objects in slab [obj0, obj1, obj2, SlabInfo]
        let mut cache: Cache<TestObjectType> = Cache::new(
            SLAB_SIZE,
            PAGE_SIZE,
            ObjectSizeType::Small,
            &mut test_memory_backend,
        )
        .expect("Failed to create cache");
        assert_eq!(cache.objects_per_slab, 3);

        // For checks
        let test_memory_backend_ref = unsafe { &mut *test_memory_backend_ptr };

        // No slabs allocated
        assert!(test_memory_backend_ref.allocated_slabs_addrs.is_empty());

        // Allocate all objects from slab
        // allocate obj2 from first slab
        let allocated_ptr = cache.alloc();
        let allocated_ptr_addr = allocated_ptr as usize;
        assert!(!allocated_ptr.is_null());
        assert!(allocated_ptr.is_aligned());
        assert_eq!(test_memory_backend_ref.allocated_slabs_addrs.len(), 1);
        assert_eq!(
            allocated_ptr_addr,
            test_memory_backend_ref.allocated_slabs_addrs[0] + size_of::<TestObjectType>() * 2
        );
        // 2 free objects in slab
        assert_eq!(
            unsafe {
                (*cache.free_slabs_list.back().get().unwrap().data.get()).free_objects_number
            },
            2
        );
        assert_eq!(
            unsafe {
                (*cache.free_slabs_list.back().get().unwrap().data.get())
                    .free_objects_list
                    .iter()
                    .count()
            },
            2
        );
        assert_eq!(
            unsafe { (*cache.free_slabs_list.back().get().unwrap().data.get()).cache_ptr },
            &cache as *const _ as *mut _
        );
        assert!(cache.full_slabs_list.is_empty());

        // allocate obj1 from first slab
        let allocated_ptr = cache.alloc();
        let allocated_ptr_addr = allocated_ptr as usize;
        assert!(!allocated_ptr.is_null());
        assert!(allocated_ptr.is_aligned());
        assert_eq!(
            allocated_ptr_addr,
            test_memory_backend_ref.allocated_slabs_addrs[0] + size_of::<TestObjectType>() * 1
        );
        // 1 free objects in free slab
        assert_eq!(
            unsafe {
                (*cache.free_slabs_list.back().get().unwrap().data.get()).free_objects_number
            },
            1
        );
        assert_eq!(
            unsafe {
                (*cache.free_slabs_list.back().get().unwrap().data.get())
                    .free_objects_list
                    .iter()
                    .count()
            },
            1
        );
        assert!(cache.full_slabs_list.is_empty());

        // allocate obj0 from first slab
        let allocated_ptr = cache.alloc();
        let allocated_ptr_addr = allocated_ptr as usize;
        assert!(!allocated_ptr.is_null());
        assert!(allocated_ptr.is_aligned());
        assert_eq!(
            allocated_ptr_addr,
            test_memory_backend_ref.allocated_slabs_addrs[0] + size_of::<TestObjectType>() * 0
        );

        // Now we have zero free slabs and one full
        assert!(cache.free_slabs_list.is_empty());
        assert_eq!(cache.full_slabs_list.iter().count(), 1);
        // 0 free objects in full slab
        assert_eq!(
            unsafe {
                (*cache.full_slabs_list.back().get().unwrap().data.get()).free_objects_number
            },
            0
        );
        assert_eq!(
            unsafe {
                (*cache.full_slabs_list.back().get().unwrap().data.get())
                    .free_objects_list
                    .iter()
                    .count()
            },
            0
        );

        // Allocate objects again
        // allocate obj2 from second slab
        let allocated_ptr = cache.alloc();
        assert!(!allocated_ptr.is_null());
        assert!(allocated_ptr.is_aligned());
        assert_eq!(
            allocated_ptr as usize,
            test_memory_backend_ref.allocated_slabs_addrs[1] + size_of::<TestObjectType>() * 2
        );
        // 2 free objects in slab
        assert_eq!(
            unsafe {
                (*cache.free_slabs_list.back().get().unwrap().data.get()).free_objects_number
            },
            2
        );
        assert_eq!(
            unsafe {
                (*cache.free_slabs_list.back().get().unwrap().data.get())
                    .free_objects_list
                    .iter()
                    .count()
            },
            2
        );
        // New slab allocated, now we have 1 free slab and 1 full
        assert_eq!(test_memory_backend_ref.allocated_slabs_addrs.len(), 2);
        assert_eq!(cache.free_slabs_list.iter().count(), 1);
        assert_eq!(cache.full_slabs_list.iter().count(), 1);

        // allocate obj1 from second slab
        let allocated_ptr = cache.alloc();
        let allocated_ptr_addr = allocated_ptr as usize;
        assert!(!allocated_ptr.is_null());
        assert!(allocated_ptr.is_aligned());
        assert_eq!(
            allocated_ptr_addr,
            test_memory_backend_ref.allocated_slabs_addrs[1] + size_of::<TestObjectType>() * 1
        );
        // 1 free object in slab
        assert_eq!(
            unsafe {
                (*cache.free_slabs_list.back().get().unwrap().data.get()).free_objects_number
            },
            1
        );
        assert_eq!(
            unsafe {
                (*cache.free_slabs_list.back().get().unwrap().data.get())
                    .free_objects_list
                    .iter()
                    .count()
            },
            1
        );

        // allocate obj0 from second slab
        let allocated_ptr = cache.alloc();
        let allocated_ptr_addr = allocated_ptr as usize;
        assert!(!allocated_ptr.is_null());
        assert!(allocated_ptr.is_aligned());
        assert_eq!(
            allocated_ptr_addr,
            test_memory_backend_ref.allocated_slabs_addrs[1] + size_of::<TestObjectType>() * 0
        );
        // Now we have zero free slabs and two full
        assert_eq!(test_memory_backend_ref.allocated_slabs_addrs.len(), 2);
        assert!(cache.free_slabs_list.is_empty());
        assert_eq!(cache.full_slabs_list.iter().count(), 2);

        // Final allocation
        // allocate obj2 from third slab
        let allocated_ptr = cache.alloc();
        let allocated_ptr_addr = allocated_ptr as usize;
        assert!(!allocated_ptr.is_null());
        assert!(allocated_ptr.is_aligned());
        assert_eq!(
            allocated_ptr_addr,
            test_memory_backend_ref.allocated_slabs_addrs[2] + size_of::<TestObjectType>() * 2
        );
        // Now we have one free slab and two full
        assert_eq!(test_memory_backend_ref.allocated_slabs_addrs.len(), 3);
        assert_eq!(cache.free_slabs_list.iter().count(), 1);
        assert_eq!(cache.full_slabs_list.iter().count(), 2);
        // Free slab contains 2 free objects
        assert_eq!(
            unsafe {
                (*cache.free_slabs_list.back().get().unwrap().data.get()).free_objects_number
            },
            2
        );
        assert_eq!(
            unsafe {
                (*cache.free_slabs_list.back().get().unwrap().data.get())
                    .free_objects_list
                    .iter()
                    .count()
            },
            2
        );
        // Full slabs don't have free objects
        for slab_info in cache.full_slabs_list.iter() {
            unsafe {
                assert_eq!((*slab_info.data.get()).free_objects_number, 0);
                assert_eq!((*slab_info.data.get()).free_objects_list.iter().count(), 0);
            }
        }
    }

    #[test]
    fn alloc_only_small_many_objects() {
        const SLAB_SIZE: usize = 16777216; // 16 MB
        const PAGE_SIZE: usize = 4096;
        struct TestMemoryBackend {
            allocated_slabs_addrs: Vec<usize>,
        }

        impl<'a, T> MemoryBackend<'a, T> for TestMemoryBackend {
            fn alloc_slab(&mut self, slab_size: usize, page_size: usize) -> *mut u8 {
                assert_eq!(slab_size, SLAB_SIZE);
                assert_eq!(page_size, PAGE_SIZE);
                let layout = Layout::from_size_align(slab_size, page_size).unwrap();
                let allocated_slab_ptr = unsafe { alloc(layout) };
                let allocated_slab_addr = allocated_slab_ptr as usize;
                self.allocated_slabs_addrs.push(allocated_slab_addr);
                unsafe { allocated_slab_ptr }
            }

            fn free_slab(&mut self, slab_ptr: *mut u8, slab_size: usize, page_size: usize) {
                unreachable!();
            }

            fn alloc_slab_info(&mut self) -> *mut SlabInfo<'a, T> {
                unreachable!();
            }

            fn free_slab_info(&mut self, slab_ptr: *mut SlabInfo<'a, T>) {
                unreachable!();
            }

            fn save_slab_info_ptr(
                &mut self,
                slab_info_ptr: *const SlabInfo<'a, T>,
                object_page_addr: usize,
            ) {
                assert_eq!(object_page_addr % PAGE_SIZE, 0);
                // Don't need in this test
            }

            fn get_slab_info_ptr(
                &mut self,
                slab_info_ptr: *const SlabInfo<'a, T>,
                object_page_addr: usize,
            ) {
                // Don't need in this test
                unreachable!();
            }
        }

        let mut test_memory_backend = TestMemoryBackend {
            allocated_slabs_addrs: Vec::new(),
        };
        let test_memory_backend_ptr = &raw mut test_memory_backend;

        struct TestObjectType16 {
            data: [u128; 1],
        }
        assert_eq!(size_of::<TestObjectType16>(), 16);

        assert_eq!(size_of::<SlabInfo<TestObjectType16>>(), 48);
        // No align required for current test SlabInfo
        assert!(((SLAB_SIZE - 48) as *const SlabInfo<TestObjectType16>).is_aligned());

        // (16777216 - 48) / 16 = 1048573
        // 1048573 objects in slab [obj0, obj1, obj2 ... SlabInfo]
        let mut cache: Cache<TestObjectType16> = Cache::new(
            SLAB_SIZE,
            PAGE_SIZE,
            ObjectSizeType::Small,
            &mut test_memory_backend,
        )
        .expect("Failed to create cache");
        assert_eq!(cache.objects_per_slab, 1048573);

        // For checks
        let test_memory_backend_ref = unsafe { &mut *test_memory_backend_ptr };

        // Allocs 1048571 objects
        for i in (2..1048573).rev() {
            let allocated_ptr = cache.alloc();
            assert_eq!(
                allocated_ptr as usize,
                test_memory_backend_ref.allocated_slabs_addrs[0] + i * 16
            );
            assert!(!allocated_ptr.is_null());
            assert!(allocated_ptr.is_aligned());
        }
        // 2 free objects
        assert!(cache.full_slabs_list.is_empty());
        assert_eq!(cache.free_slabs_list.iter().count(), 1);
        assert_eq!(test_memory_backend_ref.allocated_slabs_addrs.len(), 1);
        assert_eq!(
            unsafe {
                (*cache.free_slabs_list.back().get().unwrap().data.get()).free_objects_number
            },
            2
        );
        assert_eq!(
            unsafe {
                (*cache.free_slabs_list.back().get().unwrap().data.get())
                    .free_objects_list
                    .iter()
                    .count()
            },
            2
        );

        // Alloc 2 free objects
        let allocated_ptr = cache.alloc();
        assert!(!allocated_ptr.is_null());
        assert!(allocated_ptr.is_aligned());
        let allocated_ptr = cache.alloc();
        assert!(!allocated_ptr.is_null());
        assert!(allocated_ptr.is_aligned());

        // Zero free slabs, one full
        assert!(cache.free_slabs_list.is_empty());
        assert_eq!(cache.full_slabs_list.iter().count(), 1);
        assert_eq!(test_memory_backend_ref.allocated_slabs_addrs.len(), 1);

        // Alloc one object from new slab
        let allocated_ptr = cache.alloc();
        assert_eq!(
            allocated_ptr as usize,
            test_memory_backend_ref.allocated_slabs_addrs[1] + (1048572 * 16)
        );
        assert!(!allocated_ptr.is_null());
        assert!(allocated_ptr.is_aligned());

        // 1048572 free objects
        // 1 slab free, 1 slab full
        assert_eq!(test_memory_backend_ref.allocated_slabs_addrs.len(), 2);
        assert_eq!(cache.free_slabs_list.iter().count(), 1);
        assert_eq!(cache.full_slabs_list.iter().count(), 1);
        assert_eq!(
            unsafe {
                (*cache.free_slabs_list.back().get().unwrap().data.get()).free_objects_number
            },
            1048572
        );
    }

    #[test]
    fn alloc_only_small_single_object() {
        const SLAB_SIZE: usize = 4096;
        const PAGE_SIZE: usize = 4096;
        struct TestMemoryBackend {
            allocated_slabs_addrs: Vec<usize>,
        }

        impl<'a, T> MemoryBackend<'a, T> for TestMemoryBackend {
            fn alloc_slab(&mut self, slab_size: usize, page_size: usize) -> *mut u8 {
                assert_eq!(slab_size, SLAB_SIZE);
                assert_eq!(page_size, PAGE_SIZE);
                let layout = Layout::from_size_align(slab_size, page_size).unwrap();
                let allocated_slab_ptr = unsafe { alloc(layout) };
                let allocated_slab_addr = allocated_slab_ptr as usize;
                self.allocated_slabs_addrs.push(allocated_slab_addr);
                unsafe { allocated_slab_ptr }
            }

            fn free_slab(&mut self, slab_ptr: *mut u8, slab_size: usize, page_size: usize) {
                unreachable!();
            }

            fn alloc_slab_info(&mut self) -> *mut SlabInfo<'a, T> {
                unreachable!();
            }

            fn free_slab_info(&mut self, slab_ptr: *mut SlabInfo<'a, T>) {
                unreachable!();
            }

            fn save_slab_info_ptr(
                &mut self,
                slab_info_ptr: *const SlabInfo<'a, T>,
                object_page_addr: usize,
            ) {
                assert_eq!(object_page_addr % PAGE_SIZE, 0);
                // Don't need in this test
            }

            fn get_slab_info_ptr(
                &mut self,
                slab_info_ptr: *const SlabInfo<'a, T>,
                object_page_addr: usize,
            ) {
                // Don't need in this test
                unreachable!();
            }
        }

        let mut test_memory_backend = TestMemoryBackend {
            allocated_slabs_addrs: Vec::new(),
        };
        let test_memory_backend_ptr = &raw mut test_memory_backend;

        struct TestObjectType {
            data: [u8; 2048],
        }
        assert_eq!(size_of::<TestObjectType>(), 2048);

        // 3 objects in slab [obj0, SlabInfo]
        let mut cache: Cache<TestObjectType> = Cache::new(
            SLAB_SIZE,
            PAGE_SIZE,
            ObjectSizeType::Small,
            &mut test_memory_backend,
        )
        .expect("Failed to create cache");
        assert_eq!(cache.objects_per_slab, 1);

        // For checks
        let test_memory_backend_ref = unsafe { &mut *test_memory_backend_ptr };

        // No slabs allocated
        assert!(test_memory_backend_ref.allocated_slabs_addrs.is_empty());

        // Allocate all objects from slab
        // allocate obj0 from first slab
        let allocated_ptr = cache.alloc();
        let allocated_ptr_addr = allocated_ptr as usize;
        assert!(!allocated_ptr.is_null());
        assert!(allocated_ptr.is_aligned());
        assert_eq!(test_memory_backend_ref.allocated_slabs_addrs.len(), 1);
        assert_eq!(
            allocated_ptr_addr,
            test_memory_backend_ref.allocated_slabs_addrs[0] + size_of::<TestObjectType>() * 0
        );

        // Now we have zero free slabs and one full
        assert!(cache.free_slabs_list.is_empty());
        assert_eq!(cache.full_slabs_list.iter().count(), 1);
    }

    #[test]
    fn new_small_zero_objects_fail() {
        const SLAB_SIZE: usize = 4096;
        const PAGE_SIZE: usize = 4096;
        struct TestMemoryBackend {
            allocated_slabs_addrs: Vec<usize>,
        }

        impl<'a, T> MemoryBackend<'a, T> for TestMemoryBackend {
            fn alloc_slab(&mut self, slab_size: usize, page_size: usize) -> *mut u8 {
                assert_eq!(slab_size, SLAB_SIZE);
                assert_eq!(page_size, PAGE_SIZE);
                let layout = Layout::from_size_align(slab_size, page_size).unwrap();
                let allocated_slab_ptr = unsafe { alloc(layout) };
                let allocated_slab_addr = allocated_slab_ptr as usize;
                self.allocated_slabs_addrs.push(allocated_slab_addr);
                unsafe { allocated_slab_ptr }
            }

            fn free_slab(&mut self, slab_ptr: *mut u8, slab_size: usize, page_size: usize) {
                unreachable!();
            }

            fn alloc_slab_info(&mut self) -> *mut SlabInfo<'a, T> {
                unreachable!();
            }

            fn free_slab_info(&mut self, slab_ptr: *mut SlabInfo<'a, T>) {
                unreachable!();
            }

            fn save_slab_info_ptr(
                &mut self,
                slab_info_ptr: *const SlabInfo<'a, T>,
                object_page_addr: usize,
            ) {
                assert_eq!(object_page_addr % PAGE_SIZE, 0);
                // Don't need in this test
            }

            fn get_slab_info_ptr(
                &mut self,
                slab_info_ptr: *const SlabInfo<'a, T>,
                object_page_addr: usize,
            ) {
                // Don't need in this test
                unreachable!();
            }
        }

        let mut test_memory_backend = TestMemoryBackend {
            allocated_slabs_addrs: Vec::new(),
        };
        let test_memory_backend_ptr = &raw mut test_memory_backend;

        struct TestObjectType4096 {
            data: [u8; 4096],
        }
        // 0 objects in slab [SlabInfo]
        // Must fail!
        let mut cache = Cache::<TestObjectType4096>::new(
            SLAB_SIZE,
            PAGE_SIZE,
            ObjectSizeType::Small,
            &mut test_memory_backend,
        );
        assert!(cache.is_err());
        drop(cache);

        struct TestObjectType15 {
            data: [u8; 15],
        }
        // Too small object
        // Must fail!
        let mut cache = Cache::<TestObjectType15>::new(
            SLAB_SIZE,
            PAGE_SIZE,
            ObjectSizeType::Small,
            &mut test_memory_backend,
        );
        assert!(cache.is_err());
        drop(cache);

        struct TestObjectType16 {
            data: [u8; 16],
        }
        // Too small object
        // Must fail!
        let mut cache = Cache::<TestObjectType16>::new(
            SLAB_SIZE,
            PAGE_SIZE,
            ObjectSizeType::Small,
            &mut test_memory_backend,
        );
        assert!(cache.is_ok());
        drop(cache);
    }

    #[test]
    fn alloc_only_large() {
        const SLAB_SIZE: usize = 4096;
        const PAGE_SIZE: usize = 4096;
        struct TestMemoryBackend {
            allocated_slabs_addrs: Vec<usize>,
            allocated_slab_info_addrs: Vec<usize>,
        }

        impl<'a, T> MemoryBackend<'a, T> for TestMemoryBackend {
            fn alloc_slab(&mut self, slab_size: usize, page_size: usize) -> *mut u8 {
                assert_eq!(slab_size, SLAB_SIZE);
                assert_eq!(page_size, PAGE_SIZE);
                let layout = Layout::from_size_align(slab_size, page_size).unwrap();
                let allocated_slab_ptr = unsafe { alloc(layout) };
                let allocated_slab_addr = allocated_slab_ptr as usize;
                self.allocated_slabs_addrs.push(allocated_slab_addr);
                unsafe { allocated_slab_ptr }
            }

            fn free_slab(&mut self, slab_ptr: *mut u8, slab_size: usize, page_size: usize) {
                unreachable!();
            }

            fn alloc_slab_info(&mut self) -> *mut SlabInfo<'a, T> {
                let layout = Layout::from_size_align(
                    size_of::<SlabInfo<'a, T>>(),
                    align_of::<SlabInfo<'a, T>>(),
                )
                .unwrap();
                let allocated_slab_info_ptr = unsafe { alloc(layout).cast::<SlabInfo<'a, T>>() };
                let allocated_slab_info_addr = allocated_slab_info_ptr as usize;
                self.allocated_slab_info_addrs
                    .push(allocated_slab_info_addr);
                unsafe { allocated_slab_info_ptr }
            }

            fn free_slab_info(&mut self, slab_ptr: *mut SlabInfo<'a, T>) {
                unreachable!();
            }

            fn save_slab_info_ptr(
                &mut self,
                slab_info_ptr: *const SlabInfo<'a, T>,
                object_page_addr: usize,
            ) {
                assert_eq!(object_page_addr % PAGE_SIZE, 0);
                // Don't need in this test
            }

            fn get_slab_info_ptr(
                &mut self,
                slab_info_ptr: *const SlabInfo<'a, T>,
                object_page_addr: usize,
            ) {
                // Don't need in this test
                unreachable!();
            }
        }

        let mut test_memory_backend = TestMemoryBackend {
            allocated_slabs_addrs: Vec::new(),
            allocated_slab_info_addrs: Vec::new(),
        };
        let test_memory_backend_ptr = &raw mut test_memory_backend;

        struct TestObjectType {
            data: [u8; 1024],
        }
        assert_eq!(size_of::<TestObjectType>(), 1024);

        // 4 objects in slab [obj0, obj1, obj2, obj3]
        let mut cache: Cache<TestObjectType> = Cache::new(
            SLAB_SIZE,
            PAGE_SIZE,
            ObjectSizeType::Large,
            &mut test_memory_backend,
        )
        .expect("Failed to create cache");
        assert_eq!(cache.objects_per_slab, 4);

        // For checks
        let test_memory_backend_ref = unsafe { &mut *test_memory_backend_ptr };

        // No slabs allocated
        assert!(test_memory_backend_ref.allocated_slabs_addrs.is_empty());
        assert!(test_memory_backend_ref.allocated_slab_info_addrs.is_empty());

        // Allocate 3 objects
        // obj3, obj2, obj1
        for i in (1..4).rev() {
            let allocated_ptr = cache.alloc();
            assert!(!allocated_ptr.is_null());
            assert!(allocated_ptr.is_aligned());
            assert_eq!(
                allocated_ptr as usize,
                test_memory_backend_ref.allocated_slabs_addrs[0] + i * 1024
            );
        }
        // 1 free object
        assert_eq!(test_memory_backend_ref.allocated_slab_info_addrs.len(), 1);
        assert_eq!(cache.free_slabs_list.iter().count(), 1);
        assert_eq!(cache.full_slabs_list.iter().count(), 0);
        assert_eq!(
            unsafe {
                (*cache.free_slabs_list.back().get().unwrap().data.get())
                    .free_objects_list
                    .iter()
                    .count()
            },
            1
        );
        assert_eq!(
            unsafe {
                (*cache.free_slabs_list.back().get().unwrap().data.get()).free_objects_number
            },
            1
        );
        assert_eq!(
            unsafe { (*cache.free_slabs_list.back().get().unwrap().data.get()).cache_ptr },
            &cache as *const _ as *mut _
        );

        let allocated_ptr = cache.alloc();
        assert!(!allocated_ptr.is_null());
        assert!(allocated_ptr.is_aligned());
        assert_eq!(
            allocated_ptr as usize,
            test_memory_backend_ref.allocated_slabs_addrs[0] + 0 * 1024
        );
        // 0 free slabs and 1 allocated

        // Allocate 1 object
        let allocated_ptr = cache.alloc();
        assert!(!allocated_ptr.is_null());
        assert!(allocated_ptr.is_aligned());
        assert_eq!(
            allocated_ptr as usize,
            test_memory_backend_ref.allocated_slabs_addrs[1] + 3 * 1024
        );

        // 3 free objects
        assert_eq!(test_memory_backend_ref.allocated_slab_info_addrs.len(), 2);
        assert_eq!(cache.free_slabs_list.iter().count(), 1);
        assert_eq!(cache.full_slabs_list.iter().count(), 1);
        assert_eq!(
            unsafe {
                (*cache.free_slabs_list.back().get().unwrap().data.get())
                    .free_objects_list
                    .iter()
                    .count()
            },
            3
        );
        assert_eq!(
            unsafe {
                (*cache.free_slabs_list.back().get().unwrap().data.get()).free_objects_number
            },
            3
        );
        assert_eq!(
            unsafe { (*cache.free_slabs_list.back().get().unwrap().data.get()).cache_ptr },
            &cache as *const _ as *mut _
        );
    }

    #[test]
    fn alloc_only_large_single_object() {
        const SLAB_SIZE: usize = 4096;
        const PAGE_SIZE: usize = 4096;
        struct TestMemoryBackend {
            allocated_slabs_addrs: Vec<usize>,
            allocated_slab_info_addrs: Vec<usize>,
        }

        impl<'a, T> MemoryBackend<'a, T> for TestMemoryBackend {
            fn alloc_slab(&mut self, slab_size: usize, page_size: usize) -> *mut u8 {
                assert_eq!(slab_size, SLAB_SIZE);
                assert_eq!(page_size, PAGE_SIZE);
                let layout = Layout::from_size_align(slab_size, page_size).unwrap();
                let allocated_slab_ptr = unsafe { alloc(layout) };
                let allocated_slab_addr = allocated_slab_ptr as usize;
                self.allocated_slabs_addrs.push(allocated_slab_addr);
                unsafe { allocated_slab_ptr }
            }

            fn free_slab(&mut self, slab_ptr: *mut u8, slab_size: usize, page_size: usize) {
                unreachable!();
            }

            fn alloc_slab_info(&mut self) -> *mut SlabInfo<'a, T> {
                let layout = Layout::from_size_align(
                    size_of::<SlabInfo<'a, T>>(),
                    align_of::<SlabInfo<'a, T>>(),
                )
                .unwrap();
                let allocated_slab_info_ptr = unsafe { alloc(layout).cast::<SlabInfo<'a, T>>() };
                let allocated_slab_info_addr = allocated_slab_info_ptr as usize;
                self.allocated_slab_info_addrs
                    .push(allocated_slab_info_addr);
                unsafe { allocated_slab_info_ptr }
            }

            fn free_slab_info(&mut self, slab_ptr: *mut SlabInfo<'a, T>) {
                unreachable!();
            }

            fn save_slab_info_ptr(
                &mut self,
                slab_info_ptr: *const SlabInfo<'a, T>,
                object_page_addr: usize,
            ) {
                assert_eq!(object_page_addr % PAGE_SIZE, 0);
                // Don't need in this test
            }

            fn get_slab_info_ptr(
                &mut self,
                slab_info_ptr: *const SlabInfo<'a, T>,
                object_page_addr: usize,
            ) {
                // Don't need in this test
                unreachable!();
            }
        }

        let mut test_memory_backend = TestMemoryBackend {
            allocated_slabs_addrs: Vec::new(),
            allocated_slab_info_addrs: Vec::new(),
        };
        let test_memory_backend_ptr = &raw mut test_memory_backend;

        struct TestObjectType {
            data: [u8; 4096],
        }
        assert_eq!(size_of::<TestObjectType>(), 4096);

        // 1 object in slab [obj0]
        let mut cache: Cache<TestObjectType> = Cache::new(
            SLAB_SIZE,
            PAGE_SIZE,
            ObjectSizeType::Large,
            &mut test_memory_backend,
        )
        .expect("Failed to create cache");
        assert_eq!(cache.objects_per_slab, 1);

        // For checks
        let test_memory_backend_ref = unsafe { &mut *test_memory_backend_ptr };

        // No slabs allocated
        assert!(test_memory_backend_ref.allocated_slabs_addrs.is_empty());
        assert!(test_memory_backend_ref.allocated_slab_info_addrs.is_empty());

        // Allocate 1 object
        // obj0
        let allocated_ptr = cache.alloc();
        assert!(!allocated_ptr.is_null());
        assert!(allocated_ptr.is_aligned());
        assert_eq!(
            allocated_ptr as usize,
            test_memory_backend_ref.allocated_slabs_addrs[0] + 0 * 1024
        );

        // 0 free slabs, 1 full slab
        assert_eq!(test_memory_backend_ref.allocated_slab_info_addrs.len(), 1);
        assert_eq!(cache.free_slabs_list.iter().count(), 0);
        assert_eq!(cache.full_slabs_list.iter().count(), 1);
        assert_eq!(
            unsafe {
                (*cache.full_slabs_list.back().get().unwrap().data.get())
                    .free_objects_list
                    .iter()
                    .count()
            },
            0
        );
        assert_eq!(
            unsafe {
                (*cache.full_slabs_list.back().get().unwrap().data.get()).free_objects_number
            },
            0
        );
        assert_eq!(
            unsafe { (*cache.full_slabs_list.back().get().unwrap().data.get()).cache_ptr },
            &cache as *const _ as *mut _
        );

        // Allocate 1 object
        let allocated_ptr = cache.alloc();
        assert!(!allocated_ptr.is_null());
        assert!(allocated_ptr.is_aligned());
        assert_eq!(
            allocated_ptr as usize,
            test_memory_backend_ref.allocated_slabs_addrs[1] + 0 * 1024
        );

        // Allocate 1 object
        let allocated_ptr = cache.alloc();
        assert!(!allocated_ptr.is_null());
        assert!(allocated_ptr.is_aligned());
        assert_eq!(
            allocated_ptr as usize,
            test_memory_backend_ref.allocated_slabs_addrs[2] + 0 * 1024
        );

        // 0 free slabs, 3 full slabs
        assert_eq!(cache.free_slabs_list.iter().count(), 0);
        assert_eq!(cache.full_slabs_list.iter().count(), 3);
        for v in cache.full_slabs_list.iter() {
            let slab_info_data_ptr = v.data.get();
            unsafe {
                assert_eq!((*slab_info_data_ptr).free_objects_number, 0);
                assert_eq!(
                    (*slab_info_data_ptr).cache_ptr,
                    &cache as *const _ as *mut _
                );
            }
        }
    }
}
