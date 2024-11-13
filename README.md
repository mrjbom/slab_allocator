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
