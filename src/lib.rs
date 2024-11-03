#![no_std]
extern crate alloc;

use core::cell::UnsafeCell;
use core::ptr::{null_mut, NonNull};
use intrusive_collections::{intrusive_adapter, LinkedList, LinkedListLink};

/// Slab cache for OS

/// Slab cache
///
/// Stores objects of the type T
struct Cache<'a, T>
{
    object_size: usize,
    slab_size: usize,
    page_size: usize,
    object_size_type: ObjectSizeType,
    current_slab: Option<NonNull<SlabInfo<'a, T>>>,
    /// List of slabs full
    full_slabs_list: LinkedList<SlabInfoAdapter<'a, T>>,
    memory_backend: &'a mut dyn MemoryBackend<'a, T>,
}

impl<'a, T> Cache<'a, T>
{
    /// slab_size must be power of two
    /// size of T must be >= 16 (two pointers)
    pub fn new(
        slab_size: usize,
        page_size: usize,
        object_size_type: ObjectSizeType,
        memory_backend: &'a mut dyn MemoryBackend<'a, T>,
    ) -> Result<Self, ()> {
        let object_size = size_of::<T>();
        if object_size < size_of::<FreeObject>() {
            return Err(());
        };
        if !slab_size.is_power_of_two() || slab_size <= object_size || slab_size % page_size != 0 {
            return Err(());
        }

        Ok(Self {
            object_size,
            slab_size,
            page_size,
            object_size_type,
            current_slab: None,
            full_slabs_list: LinkedList::new(SlabInfoAdapter::new()),
            memory_backend,
        })
    }

    /// Allocs object from cache
    pub fn alloc(&mut self) -> *mut T {
        if self.current_slab.is_none() {
            // Need to allocate new slab
            let slab_ptr = self.memory_backend.alloc_slab(self.slab_size);
            if slab_ptr.is_null() {
                return null_mut();
            }

            let slab_info_ptr = match self.object_size_type {
                ObjectSizeType::Small => {
                    // Place SlabInfo inside slab, at end
                    let slab_end_addr = slab_ptr as usize + self.slab_size;
                    let slab_info_addr = (slab_end_addr - size_of::<SlabInfo<T>>()) & !(align_of::<SlabInfo<T>>() - 1);
                    debug_assert_eq!(slab_info_addr % align_of::<SlabInfo<T>>(), 0);
                    debug_assert!(slab_info_addr > slab_ptr as usize && slab_info_addr <= slab_end_addr - size_of::<SlabInfo<T>>());
                    slab_info_addr as *mut SlabInfo<T>
                }
                ObjectSizeType::Large => {
                    // Allocate SlabInfo
                    unsafe { self.memory_backend.alloc_slab_info() }
                }
            };
            if slab_info_ptr.is_null() {
                // Failed to allocate SlabInfo
                self.memory_backend.free_slab(slab_ptr, self.slab_size);
                return null_mut();
            }
            debug_assert_eq!(slab_info_ptr as usize % align_of::<SlabInfo<T>>(), 0);

            // Fill free objects list
            let mut free_objects_list = LinkedList::new(FreeObjectAdapter::new());
            let objects_number = {
                match self.object_size_type {
                    ObjectSizeType::Small => {
                        // Slab stored inside slab
                        let used_by_slab_info = slab_ptr as usize + self.slab_size - slab_info_ptr as usize;
                        (self.slab_size - used_by_slab_info) / self.object_size
                    }
                    ObjectSizeType::Large => {
                        self.slab_size / self.object_size
                    }
                }
            };
            for free_object_index in 0..objects_number {
                // Free objects linked list element data stored inside free object
                let free_object_ptr = (free_object_index * size_of::<T>() + slab_ptr as usize) as *mut FreeObject;
                unsafe {
                    debug_assert_eq!(free_object_ptr as usize % align_of::<T>(), 0);
                    free_object_ptr.write(FreeObject {
                        free_object_link: LinkedListLink::new(),
                    });
                    let free_object_ref = &*free_object_ptr;
                    free_objects_list.push_back(free_object_ref);
                }
            }

            // Write slab info
            unsafe {
                *slab_info_ptr = SlabInfo {
                    slab_link: LinkedListLink::new(),
                    data: UnsafeCell::new(SlabInfoData {
                        free_objects_list,
                        cache_ptr: self as *mut Self,
                        free_objects_number: objects_number,
                    }),
                    objects_number,
                }
            };

            // Set allocated slab as current
            debug_assert_eq!(slab_info_ptr as usize % align_of::<SlabInfo<T>>(), 0);
            self.current_slab = Some(NonNull::new(slab_info_ptr).unwrap());
        }
        // Allocate object

        null_mut()
    }

    /// Returns object to cache
    pub fn free(&mut self, object_ptr: *mut T) {
        unimplemented!();
    }
}

#[derive(Debug)]
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
    /// Total objects in slub
    objects_number: usize,
}

struct SlabInfoData<'a, T> {
    /// Free objects in slab list
    free_objects_list: LinkedList<FreeObjectAdapter<'a>>,
    /// Slab cache to which slab belongs
    cache_ptr: *mut Cache<'a, T>,
    /// Free objects number in slab
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
                let layout = Layout::from_size_align(slab_size, self.page_size).unwrap();
                unsafe { dealloc(slab_ptr, layout) };
            }

            fn alloc_slab_info(&mut self) -> *mut SlabInfo<'a, T> {
                unreachable!();
            }

            fn free_slab_info(&mut self, slab_ptr: *mut SlabInfo<T>) {
                unreachable!();
            }
        }
        let mut test_memory_backend = TestMemoryBackend { page_size: 4096 };

        struct SomeType {
            a: usize,
            b: usize,
        }

        let mut slab_cache = Cache::<SomeType>::new(
            4096,
            test_memory_backend.page_size,
            ObjectSizeType::Small,
            &mut test_memory_backend,
        )
        .expect("Failed to create cache");
        let allocated_ptr = slab_cache.alloc();
        assert!(!allocated_ptr.is_null());
        slab_cache.free(allocated_ptr);
    }
}
