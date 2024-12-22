# Slab Allocator

Allocator designed to allocate objects of the same size in the kernel.  
Pretty simple and was developed for my kernel, so it is supposed to be used on x86_64. It was also tested with regard to the parameters of this platform.

## How to use:
Slab Allocator uses Memory Backend to allocate and free memory. Memory Backend must implement some functions, the list of required functions differs in different allocator configurations.

Slab Size and Page Size | ObjectSizeType | Required Memory Backend Functions |
| - | - | - |
| slab_size == page_size | Small | Slab alloc/free |
| slab_size > page_size | Small | Slab alloc/free, SlabInfo ptr save/get |
| slab_size >= page_size | Large | Slab alloc/free, SlabInfo ptr save/get, SlabInfo alloc/free |

The difference between `ObjectSizeType::Small` and `ObjectSizeType::Large`:  
With `ObjectSizeType::Small` `SlabInfo` is stored inside Slab, while with `ObjectSizeType::Large` it is allocated via MemoryBackend. In case of `ObjectSizeType::Small`, some memory is occupied by `SlabInfo` and if the object size is large enough, all its memory will be lost. In other words, if for example we create a 4KB Slab and store 2KB objects, then instead of two objects, the Slab will contain only one and 50% of memory will be lost due to `SlabInfo` storage. More details can be found in the comments to `ObjectSizeType`.  
A good way to allocate `SlabInfo` is to use the Slab Allocator itself. That is, we simply create a `slab_allocator::Cache` of type `slab_size == page_size && ObjectSizeType::Small` and use it.

About saving/getting `SlabInfo` ptr:
Only in the case of `slab_size == page_size && ObjectSizeType::Small` configuration can the allocator calculate the `SlabInfo` position itself, in all other cases it will want to save and get the `SlabInfo` position from `MemoryBackend`.  
A good way to save and get this position is to use a hash table.

Important. The save/get `SlabInfo` functions are called at each alloc/free, so it is important to make them fast.

## Additional
I spent most of the development writing tests, the allocator seems pretty well tested, I think my schizophrenia made me test almost everything. It's also tested with random tests and miri.

I haven't tested its performance, but since it uses a doubly-linked list everywhere, it should be fast enough. Especially if the SlabInfo save/get functions are fast or not used at all.

Unlike Bonwick allocator, this one does not have a contructor and destructor for objects, but only allocates Slab's memory.

## Example

```
use slab_allocator::{Cache, MemoryBackend, ObjectSizeType, SlabInfo};
use std::alloc::{alloc, dealloc, Layout};
use std::collections::HashMap;

// Memory Backend for allocator
struct AllocatorMemoryBackend {
    saved_slab_infos: HashMap<usize, *mut SlabInfo>,
}

impl MemoryBackend for AllocatorMemoryBackend {
    unsafe fn alloc_slab(&mut self, slab_size: usize, page_size: usize) -> *mut u8 {
        let layout = Layout::from_size_align(slab_size, page_size).unwrap();
        alloc(layout)
    }

    unsafe fn free_slab(&mut self, slab_ptr: *mut u8, slab_size: usize, page_size: usize) {
        let layout = Layout::from_size_align(slab_size, page_size).unwrap();
        dealloc(slab_ptr, layout);
    }

    unsafe fn alloc_slab_info(&mut self) -> *mut SlabInfo {
        let layout = Layout::new::<SlabInfo>();
        alloc(layout).cast()
    }

    unsafe fn free_slab_info(&mut self, slab_info_ptr: *mut SlabInfo) {
        let layout = Layout::new::<SlabInfo>();
        dealloc(slab_info_ptr.cast(), layout);
    }

    unsafe fn save_slab_info_ptr(
        &mut self,
        object_page_addr: usize,
        slab_info_ptr: *mut SlabInfo,
    ) {
        self.saved_slab_infos
            .insert(object_page_addr, slab_info_ptr);
    }

    unsafe fn get_slab_info_ptr(&mut self, object_page_addr: usize) -> *mut SlabInfo {
        *self.saved_slab_infos.get(&object_page_addr).unwrap()
    }

    unsafe fn delete_slab_info_ptr(&mut self, page_addr: usize) {
        if self.saved_slab_infos.contains_key(&page_addr) {
            self.saved_slab_infos.remove(&page_addr);
        }
    }
}

fn main() {
    // Create memory backend
    let allocator_memory_backend = AllocatorMemoryBackend {
        saved_slab_infos: HashMap::new(),
    };

    struct SomeType {
        num: u64,
        condition: bool,
        array: [u8; 4],
    };

    const SLAB_SIZE: usize = 8192;
    const PAGE_SIZE: usize = 4096;
    const OBJECT_SIZE_TYPE: ObjectSizeType = ObjectSizeType::Large;

    // Create cache
    let mut cache = Cache::<SomeType, AllocatorMemoryBackend>::new(
        SLAB_SIZE,
        PAGE_SIZE,
        OBJECT_SIZE_TYPE,
        allocator_memory_backend,
    )
    .unwrap_or_else(|error| panic!("Failed to create cache: {error}"));

    unsafe {
        // Allocate
        let p1: *mut SomeType = cache.alloc();
        assert!(!p1.is_null() && p1.is_aligned());
        let p2: *mut SomeType = cache.alloc();
        assert!(!p2.is_null() && p2.is_aligned());

        // Free
        cache.free(p1);
        cache.free(p2);
    }
}
```
