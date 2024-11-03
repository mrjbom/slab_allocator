#![no_std]

use intrusive_collections::{LinkedList, LinkedListLink, intrusive_adapter};

/// Slab cache for OS

/// Slab cache
///
/// Stores objects of the same type
struct SlabCache<'a, 'b, const PAGE_SIZE: usize = 4096> {
    object_size: usize,
    slab_size: usize,
    object_size_type: ObjectSizeType,
    /// List of slabs with free objects
    free_slabs_list: LinkedList<SlabInfoAdapter<'b>>,
    /// List of slabs full
    full_slabs_list: LinkedList<SlabInfoAdapter<'b>>,
    memory_backend: &'a dyn MemoryBackend,
}

impl<'a> SlabCache<'a, 'a> {
    /// slab_size must be power of two
    pub fn new(object_size: usize, slab_size: usize, object_size_type: ObjectSizeType, memory_backend: &'a dyn MemoryBackend) -> Result<Self, ()> {
        if object_size == 0 || slab_size == 0 || !slab_size.is_power_of_two() {
            return Err(());
        }
        Ok(Self {
            object_size,
            slab_size,
            object_size_type,
            free_slabs_list: LinkedList::new(SlabInfoAdapter::new()),
            full_slabs_list: LinkedList::new(SlabInfoAdapter::new()),
            memory_backend,
        })
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
struct SlabInfo<'a> {
    /// Link to next and prev slab
    slab_link: LinkedListLink,
    /// Slab size
    slab_size: usize,
    /// Free objects in slab list
    free_objects_list: LinkedList<FreeObjectAdapter<'a>>,
    /// Slab cache to which slab belongs
    slab_cache: &'a SlabCache<'a, 'a>,
}

#[derive(Debug)]
#[repr(transparent)]
/// Metadata stored inside a free object and pointing to the previous and next free object
struct FreeObject {
    free_object_link: LinkedListLink,
}

intrusive_adapter!(SlabInfoAdapter<'a> = &'a SlabInfo<'a>: SlabInfo { slab_link: LinkedListLink });
intrusive_adapter!(FreeObjectAdapter<'a> = &'a FreeObject: FreeObject { free_object_link: LinkedListLink });

/// Used by slab cache for allocating slabs and SlabInfo's
///
/// Slab caching logic can be placed here
///
/// alloc_slab_info() and free_slab_info() not used by small objects cache and can always return null
trait MemoryBackend
{
    /// Allocates slab for cache
    ///
    /// Must be page aligned
    fn alloc_slab(&mut self, size: usize, page_size: usize) -> *mut u8;

    /// Frees slab
    fn free_slab(&mut self, slab_ptr: *mut u8);

    /// Allocs SlabInfo
    ///
    /// Not used by small object cache and can always return null
    fn alloc_slab_info(&mut self) -> *mut SlabInfo;

    /// Frees SlabInfo
    ///
    /// Not used by small object cache and can always return null
    fn free_slab_info(&mut self, slab_ptr: *mut SlabInfo);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test() {

    }
}
