#![no_std]
extern crate alloc;

use intrusive_collections::{LinkedList, LinkedListLink, intrusive_adapter};

/// Slab cache for OS

/// Slab cache
///
/// Stores objects of the type T
struct Cache<'a, 'b, T> {
    object_size: usize,
    slab_size: usize,
    page_size: usize,
    object_size_type: ObjectSizeType,
    /// List of slabs with free objects
    free_slabs_list: LinkedList<SlabInfoAdapter<'b, T>>,
    /// List of slabs full
    full_slabs_list: LinkedList<SlabInfoAdapter<'b, T>>,
    memory_backend: &'a dyn MemoryBackend<T>,
}

impl<'a, T> Cache<'a, '_, T> {
    /// slab_size must be power of two
    /// size of T must be >= 16 (two pointers)
    pub fn new(slab_size: usize, page_size: usize, object_size_type: ObjectSizeType, memory_backend: &'a mut dyn MemoryBackend<T>) -> Result<Self, ()> {
        let object_size = size_of::<T>();
        if object_size < size_of::<FreeObject>() {
            return Err(());
        };
        if !slab_size.is_power_of_two() || slab_size <= object_size {
            return Err(());
        }

        let slab = memory_backend.alloc_slab(slab_size);

        Ok(Self {
            object_size,
            slab_size,
            page_size,
            object_size_type,
            free_slabs_list: LinkedList::new(SlabInfoAdapter::new()),
            full_slabs_list: LinkedList::new(SlabInfoAdapter::new()),
            memory_backend,
        })
    }

    /// Allocs object
    pub fn alloc(&mut self) -> *mut T {
        unimplemented!();
    }

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
    /// Slab size
    slab_size: usize,
    /// Free objects in slab list
    free_objects_list: LinkedList<FreeObjectAdapter<'a>>,
    /// Slab cache to which slab belongs
    slab_cache: &'a Cache<'a, 'a, T>,
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
trait MemoryBackend<T>
{
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
    fn alloc_slab_info(&mut self) -> *mut SlabInfo<T>;

    /// Frees SlabInfo
    ///
    /// Not used by small object cache and can always return null
    fn free_slab_info(&mut self, slab_ptr: *mut SlabInfo<T>);
}

#[cfg(test)]
mod tests {
    use alloc::alloc::{ Layout, alloc, dealloc };
    use super::*;
    #[test]
    fn alloc_from_small() {
        struct TestMemoryBackend {
            page_size: usize,
        };
        impl<T> MemoryBackend<T> for TestMemoryBackend {
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

            fn alloc_slab_info(&mut self) -> *mut SlabInfo<T> {
                unreachable!();
            }

            fn free_slab_info(&mut self, slab_ptr: *mut SlabInfo<T>) {
                unreachable!();
            }
        }
        let mut test_memory_backend = TestMemoryBackend {
            page_size: 4096,
        };

        struct SomeType {
            a: usize,
            b: usize,
        }

        let mut slab_cache = Cache::<SomeType>::new(4096, test_memory_backend.page_size, ObjectSizeType::Small, &mut test_memory_backend).expect("Failed to create cache");
        let allocated_ptr = slab_cache.alloc();
        assert!(!allocated_ptr.is_null());
        slab_cache.free(allocated_ptr);
    }
}
