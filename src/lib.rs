//#![no_std]
#![allow(unused)]

use core::cell::UnsafeCell;
use core::ptr::null_mut;
use intrusive_collections::{intrusive_adapter, LinkedList, LinkedListLink};

/// Slab cache for OS

/// Slab cache
///
/// Stores objects of the type T
struct Cache<'a, T> {
    object_size: usize,
    slab_size: usize,
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
    /// slab_size must be power of two
    /// size of T must be >= 16 (two pointers)
    pub fn new(
        slab_size: usize,
        object_size_type: ObjectSizeType,
        memory_backend: &'a mut dyn MemoryBackend<'a, T>,
    ) -> Result<Self, ()> {
        let object_size = size_of::<T>();
        if object_size < size_of::<FreeObject>() {
            return Err(());
        };
        if !slab_size.is_power_of_two() || slab_size <= object_size {
            return Err(());
        }

        // Calculate number of objects in slab
        let objects_per_slab = match object_size_type {
            ObjectSizeType::Small => {
                let fake_slab_addr = 0usize;
                let fake_slab_info_addr = get_slab_info_addr_in_small_object_cache::<T>(
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

        Ok(Self {
            object_size,
            slab_size,
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
            let slab_ptr = self.memory_backend.alloc_slab(self.slab_size);
            if slab_ptr.is_null() {
                return null_mut();
            }

            // Calculate/allocate SlabInfo ptr
            let slab_info_ptr = match self.object_size_type {
                ObjectSizeType::Small => {
                    // SlabInfo stored inside slab, at end
                    let slab_info_addr =
                        get_slab_info_addr_in_small_object_cache::<T>(slab_ptr, self.slab_size);
                    debug_assert!(slab_info_addr > slab_ptr as usize);
                    debug_assert!(
                        slab_info_addr
                            <= slab_ptr as usize + self.slab_size - size_of::<SlabInfo<T>>()
                    );

                    slab_info_addr as *mut SlabInfo<T>
                }
                ObjectSizeType::Large => {
                    // Allocate memory using memory backend
                    // Save ptr to OS "page" struct
                    unimplemented!();
                }
            };
            if slab_info_ptr.is_null() {
                // Failed to allocate SlabInfo
                self.memory_backend.free_slab(slab_ptr, self.slab_size);
                return null_mut();
            }
            assert_eq!(
                slab_info_ptr as usize % align_of::<SlabInfo<T>>(),
                0,
                "SlabInfo addr not aligned!"
            );

            // Make SlabInfo ref
            unsafe { slab_info_ptr.write_bytes(0, 1) };
            let slab_info_ref = unsafe { &mut *slab_info_ptr };
            let slab_info_data_ref = unsafe { &mut *slab_info_ref.data.get() };
            // Init SlabInfo
            *slab_info_ref = SlabInfo {
                slab_link: LinkedListLink::new(),
                data: UnsafeCell::new(SlabInfoData {
                    free_objects_list: LinkedList::new(FreeObjectAdapter::new()),
                    cache_ptr: self as *mut Self,
                    free_objects_number: self.objects_per_slab,
                }),
            };
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
                    *free_object_ptr = FreeObject {
                        free_object_link: LinkedListLink::new(),
                    };
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

        // Check free objects list
        if free_slab_info_data.free_objects_list.is_empty() {
            // Slab is empty now
            // Remove from free list
            let free_slab_info_addr = free_slab_info as *const _ as usize;
            let free_slab_info1 = self.free_slabs_list.pop_back().unwrap();
            assert_eq!(free_slab_info1 as *const _ as usize, free_slab_info_addr);
            // Add to full list
            self.full_slabs_list.push_back(free_slab_info1);
        }

        free_object_ptr
    }

    /// Returns object to cache
    pub fn free(&mut self, object_ptr: *mut T) {
        unimplemented!();
    }
}

fn get_slab_info_addr_in_small_object_cache<T>(slab_ptr: *mut u8, slab_size: usize) -> usize {
    // SlabInfo inside slab, at end
    let slab_end_addr = slab_ptr as usize + slab_size;
    (slab_end_addr - size_of::<SlabInfo<T>>()) & !(align_of::<SlabInfo<T>>() - 1)
}

#[derive(Debug, Copy, Clone)]
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
    /// slab_size always power of two and greater than page_size
    ///
    /// For example: page_size * 1, page_size * 2, page_size * 4, ...
    ///
    /// Must be page aligned
    fn alloc_slab(&mut self, slab_size: usize) -> *mut u8;

    /// Frees slab
    fn free_slab(&mut self, slab_ptr: *mut u8, slab_size: usize);

    /// Allocs SlabInfo
    ///
    /// Not used by small object cache and can always return null
    fn alloc_slab_info(&mut self) -> *mut SlabInfo<'a, T>;

    /// Frees SlabInfo
    ///
    /// Not used by small object cache and can always return null
    fn free_slab_info(&mut self, slab_ptr: *mut SlabInfo<'a, T>);
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate alloc;
    use alloc::alloc::{alloc, dealloc, Layout};
    #[test]
    fn alloc_from_small() {
        struct TestMemoryBackend {
            page_size: usize,
        }
        impl<'a, T> MemoryBackend<'a, T> for TestMemoryBackend {
            fn alloc_slab(&mut self, slab_size: usize) -> *mut u8 {
                assert!(slab_size >= self.page_size);
                assert!(slab_size.is_power_of_two());
                let layout = Layout::from_size_align(slab_size, self.page_size).unwrap();
                unsafe { alloc(layout) }
            }

            fn free_slab(&mut self, slab_ptr: *mut u8, slab_size: usize) {
                assert_eq!(slab_ptr as usize % 4096, 0);
                assert!(slab_size >= self.page_size);
                assert!(slab_size.is_power_of_two());
                let layout = Layout::from_size_align(slab_size, self.page_size).unwrap();
                unsafe { dealloc(slab_ptr, layout) };
            }

            fn alloc_slab_info(&mut self) -> *mut SlabInfo<'a, T> {
                unreachable!();
            }

            fn free_slab_info(&mut self, _slab_ptr: *mut SlabInfo<T>) {
                unreachable!();
            }
        }
        let mut test_memory_backend = TestMemoryBackend { page_size: 4096 };

        struct SomeType {
            a: usize,
            b: usize,
        }

        let mut slab_cache =
            Cache::<SomeType>::new(4096, ObjectSizeType::Small, &mut test_memory_backend)
                .expect("Failed to create cache");

        for _ in 0..slab_cache.objects_per_slab - 1 {
            let allocated_ptr = slab_cache.alloc();
            assert!(!allocated_ptr.is_null());
        }
        let allocated_ptr = slab_cache.alloc();
        assert!(!allocated_ptr.is_null());
        //slab_cache.free(allocated_ptr);
    }
}
