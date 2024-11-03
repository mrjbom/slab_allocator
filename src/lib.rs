#![no_std]

use intrusive_collections::{LinkedList, LinkedListLink, intrusive_adapter};

/// Slab cache for OS

#[derive(Debug)]
/// Slab cache
///
/// Stores objects of the same type
struct SlabCache<const PAGE_SIZE: usize = 4096> {
    object_size: usize,
    slab_size: usize,
}

#[repr(C)]
/// Slab info
///
/// Stored in slab(for small objects slab) or allocatated from another slab
struct SlabInfo<'a> {
    /// Link to next and prev slab
    slab_link: LinkedListLink,
    /// Slab size
    slab_size: usize,
    /// Free objects in slab list
    free_objects: LinkedList<SlabInfoAdapter<'a>>,
    /// Slab cache to which slab belongs
    slab_cache: &'a SlabCache,
}

intrusive_adapter!(SlabInfoAdapter<'a> = &'a SlabInfo<'a>: SlabInfo { slab_link: LinkedListLink });

#[derive(Debug)]
#[repr(transparent)]
/// Metadata stored inside a free object and pointing to the previous and next free object
struct FreeObject {
    free_object_link: LinkedListLink,
}

intrusive_adapter!(FreeObjectAdapter<'a> = &'a FreeObject: FreeObject { free_object_link: LinkedListLink });

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test() {

    }
}
