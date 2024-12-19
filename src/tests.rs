#[cfg(test)]
mod tests {
    use crate::*;
    extern crate alloc;
    extern crate std;
    use alloc::alloc::{alloc, dealloc, Layout};
    use alloc::vec;
    use alloc::vec::Vec;
    use rand::prelude::SliceRandom;
    use rand::{thread_rng, Rng};
    use spin::{Mutex, Once};
    use std::collections::{HashMap, HashSet};

    #[test]
    fn can_be_used_as_static() {
        let test_memory_backend: TestMemoryBackend = TestMemoryBackend;

        static CACHE: Once<Mutex<Cache<i128, TestMemoryBackend>>> = Once::new();

        CACHE.call_once(|| {
            Mutex::new(Cache::new(4096, 4096, ObjectSizeType::Small, test_memory_backend).unwrap())
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
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 1);
            assert_eq!(cache.free_slabs_list_occupacy_more_75.iter().count(), 0);
            assert_eq!(cache.full_slabs_list.iter().count(), 2);
            // 2 free objects
            assert_eq!(
                (*cache
                    .free_slabs_list_occupacy_less_75
                    .back()
                    .get()
                    .unwrap()
                    .data
                    .get())
                .free_objects_list
                .iter()
                .count(),
                2
            );
            assert_eq!(
                (*cache
                    .free_slabs_list_occupacy_less_75
                    .back()
                    .get()
                    .unwrap()
                    .data
                    .get())
                .free_objects_number,
                2
            );

            // Alloc 2
            assert!(!cache.alloc().is_null());
            assert!(!cache.alloc().is_null());
            // 0 free, 3 full slabs
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 0);
            assert_eq!(cache.free_slabs_list_occupacy_more_75.iter().count(), 0);
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
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 1);
            assert_eq!(cache.free_slabs_list_occupacy_more_75.iter().count(), 0);
            assert_eq!(cache.full_slabs_list.iter().count(), 3);
            // 3 free objects
            assert_eq!(
                (*cache
                    .free_slabs_list_occupacy_less_75
                    .back()
                    .get()
                    .unwrap()
                    .data
                    .get())
                .free_objects_list
                .iter()
                .count(),
                3
            );
            assert_eq!(
                (*cache
                    .free_slabs_list_occupacy_less_75
                    .back()
                    .get()
                    .unwrap()
                    .data
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
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 1);
            assert_eq!(cache.free_slabs_list_occupacy_more_75.iter().count(), 0);
            assert_eq!(cache.full_slabs_list.iter().count(), 1);
            // 46 free objects
            assert_eq!(
                (*cache
                    .free_slabs_list_occupacy_less_75
                    .back()
                    .get()
                    .unwrap()
                    .data
                    .get())
                .free_objects_list
                .iter()
                .count(),
                46
            );
            assert_eq!(
                (*cache
                    .free_slabs_list_occupacy_less_75
                    .back()
                    .get()
                    .unwrap()
                    .data
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
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 1);
            assert_eq!(cache.free_slabs_list_occupacy_more_75.iter().count(), 0);
            assert_eq!(cache.full_slabs_list.iter().count(), 0);
            // 412 free objects
            assert_eq!(
                (*cache
                    .free_slabs_list_occupacy_less_75
                    .back()
                    .get()
                    .unwrap()
                    .data
                    .get())
                .free_objects_list
                .iter()
                .count(),
                412
            );
            assert_eq!(
                (*cache
                    .free_slabs_list_occupacy_less_75
                    .back()
                    .get()
                    .unwrap()
                    .data
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
            assert!(cache.free_slabs_list_occupacy_less_75.is_empty());
            assert!(cache.free_slabs_list_occupacy_more_75.is_empty());
            assert!(cache.memory_backend.allocated_slab_addrs.is_empty());

            // Alloc first slab particaly
            let mut first_slab_ptrs = vec![null_mut(); cache.objects_per_slab - 1];
            for v in first_slab_ptrs.iter_mut() {
                *v = cache.alloc();
                assert!(!v.is_null());
                assert!(v.is_aligned());
            }

            // 1 free slab, 0 full slab
            assert_eq!(cache.free_slabs_list_occupacy_more_75.iter().count(), 1);
            assert_eq!(cache.full_slabs_list.iter().count(), 0);

            // Alloc last object
            first_slab_ptrs.push(cache.alloc());
            assert!(!first_slab_ptrs.last().unwrap().is_null());
            assert!(first_slab_ptrs.last().unwrap().is_aligned());

            let first_slab_ptrs_copy = first_slab_ptrs.clone();

            // 0 free slabs, 1 full
            assert!(cache.free_slabs_list_occupacy_less_75.is_empty());
            assert!(cache.free_slabs_list_occupacy_more_75.is_empty());
            assert_eq!(cache.full_slabs_list.iter().count(), 1);

            // Mix addresses
            first_slab_ptrs.shuffle(&mut rand::thread_rng());

            // Free all objects except one
            let len = first_slab_ptrs.len() - 1;
            for _ in 0..len {
                cache.free(first_slab_ptrs.pop().unwrap());
            }
            // 1 free slabs, 0 full
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 1);
            assert!(cache.free_slabs_list_occupacy_more_75.is_empty());
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
            assert!(cache.free_slabs_list_occupacy_less_75.is_empty());
            assert!(cache.free_slabs_list_occupacy_more_75.is_empty());
            assert_eq!(cache.full_slabs_list.iter().count(), 1);

            // Alloc 0.5 slab
            let mut second_slab_ptrs = Vec::new();
            for _ in 0..cache.objects_per_slab / 2 {
                second_slab_ptrs.push(cache.alloc());
                assert!(!second_slab_ptrs.last().unwrap().is_null());
                assert!(second_slab_ptrs.last().unwrap().is_aligned());
            }

            // 1 free slabs, 1 full slabs
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 1);
            assert!(cache.free_slabs_list_occupacy_more_75.is_empty());
            assert_eq!(cache.full_slabs_list.iter().count(), 1);

            // Free first slab
            first_slab_ptrs.shuffle(&mut rand::thread_rng());
            for v in first_slab_ptrs.iter() {
                cache.free(*v);
            }

            // 1 free slabs, 0 full slabs
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 1);
            assert_eq!(cache.full_slabs_list.iter().count(), 0);

            // Free second slab
            second_slab_ptrs.shuffle(&mut rand::thread_rng());
            for v in second_slab_ptrs.iter() {
                cache.free(*v);
            }

            // 0 free slabs, 0 full slabs
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 0);
            assert_eq!(cache.free_slabs_list_occupacy_more_75.iter().count(), 0);
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
                for free_slab_info in cache
                    .free_slabs_list_occupacy_less_75
                    .iter()
                    .chain(cache.free_slabs_list_occupacy_more_75.iter())
                {
                    free_objects_counter += (*free_slab_info.data.get()).free_objects_number;
                }
                assert_eq!(cache.statistics.free_objects_number, free_objects_counter);
                assert_eq!(
                    cache.statistics.full_slabs_number,
                    cache.full_slabs_list.iter().count()
                );
                assert_eq!(
                    cache.statistics.free_slabs_number,
                    cache.free_slabs_list_occupacy_less_75.iter().count()
                        + cache.free_slabs_list_occupacy_more_75.iter().count()
                );

                // Free all objects
                allocated_ptrs.shuffle(&mut rand::thread_rng());
                for v in allocated_ptrs.into_iter() {
                    cache.free(v);
                }
                assert_eq!(cache.memory_backend.allocated_slab_addrs.len(), 0);
            }

            assert!(cache.free_slabs_list_occupacy_less_75.is_empty());
            assert!(cache.free_slabs_list_occupacy_more_75.is_empty());
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
            assert!(cache.free_slabs_list_occupacy_less_75.is_empty());
            assert!(cache.free_slabs_list_occupacy_more_75.is_empty());
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
            assert_eq!(cache.free_slabs_list_occupacy_more_75.iter().count(), 1);
            assert_eq!(cache.full_slabs_list.iter().count(), 0);

            // Alloc last object
            first_slab_ptrs.push(cache.alloc());
            assert!(!first_slab_ptrs.last().unwrap().is_null());
            assert!(first_slab_ptrs.last().unwrap().is_aligned());

            let first_slab_ptrs_copy = first_slab_ptrs.clone();

            // 0 free slabs, 1 full
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 0);
            assert_eq!(cache.free_slabs_list_occupacy_more_75.iter().count(), 0);
            assert_eq!(cache.full_slabs_list.iter().count(), 1);

            // Mix addresses
            first_slab_ptrs.shuffle(&mut rand::thread_rng());

            // Free all objects except one
            let len = first_slab_ptrs.len() - 1;
            for _ in 0..len {
                cache.free(first_slab_ptrs.pop().unwrap());
            }
            // 1 free slabs, 0 full
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 1);
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
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 0);
            assert_eq!(cache.free_slabs_list_occupacy_more_75.iter().count(), 0);
            assert_eq!(cache.full_slabs_list.iter().count(), 1);

            // Alloc 0.5 slab
            let mut second_slab_ptrs = Vec::new();
            for _ in 0..cache.objects_per_slab / 2 {
                second_slab_ptrs.push(cache.alloc());
                assert!(!second_slab_ptrs.last().unwrap().is_null());
                assert!(second_slab_ptrs.last().unwrap().is_aligned());
            }

            // 1 free slabs, 1 full slabs
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 1);
            assert_eq!(cache.full_slabs_list.iter().count(), 1);

            // Free first slab
            first_slab_ptrs.shuffle(&mut rand::thread_rng());
            for v in first_slab_ptrs.iter() {
                cache.free(*v);
            }

            // 1 free slabs, 0 full slabs
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 1);
            assert_eq!(cache.full_slabs_list.iter().count(), 0);

            // Free second slab
            second_slab_ptrs.shuffle(&mut rand::thread_rng());
            for v in second_slab_ptrs.iter() {
                cache.free(*v);
            }

            // All memory free
            // 0 free slabs, 0 full slabs
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 0);
            assert_eq!(cache.free_slabs_list_occupacy_more_75.iter().count(), 0);
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
                for free_slab_info in cache
                    .free_slabs_list_occupacy_less_75
                    .iter()
                    .chain(cache.free_slabs_list_occupacy_more_75.iter())
                {
                    free_objects_counter += (*free_slab_info.data.get()).free_objects_number;
                }
                assert_eq!(cache.statistics.free_objects_number, free_objects_counter);
                assert_eq!(
                    cache.statistics.full_slabs_number,
                    cache.full_slabs_list.iter().count()
                );
                assert_eq!(
                    cache.statistics.free_slabs_number,
                    cache
                        .free_slabs_list_occupacy_less_75
                        .iter()
                        .chain(cache.free_slabs_list_occupacy_more_75.iter())
                        .count()
                );

                // Free all objects
                allocated_ptrs.shuffle(&mut rand::thread_rng());
                for v in allocated_ptrs.into_iter() {
                    cache.free(v);
                }
                assert_eq!(cache.memory_backend.allocated_slab_addrs.len(), 0);
            }

            assert!(cache.free_slabs_list_occupacy_less_75.is_empty());
            assert!(cache.free_slabs_list_occupacy_more_75.is_empty());
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
            assert!(cache.free_slabs_list_occupacy_less_75.is_empty());
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
            assert_eq!(cache.free_slabs_list_occupacy_more_75.iter().count(), 1);
            assert_eq!(cache.full_slabs_list.iter().count(), 0);

            // Alloc last object
            first_slab_ptrs.push(cache.alloc());
            assert!(!first_slab_ptrs.last().unwrap().is_null());
            assert!(first_slab_ptrs.last().unwrap().is_aligned());

            let first_slab_ptrs_copy = first_slab_ptrs.clone();

            // 0 free slabs, 1 full
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 0);
            assert_eq!(cache.free_slabs_list_occupacy_more_75.iter().count(), 0);
            assert_eq!(cache.full_slabs_list.iter().count(), 1);

            // Mix addresses
            first_slab_ptrs.shuffle(&mut rand::thread_rng());

            // Free all objects except one
            let len = first_slab_ptrs.len() - 1;
            for _ in 0..len {
                cache.free(first_slab_ptrs.pop().unwrap());
            }
            // 1 free slabs, 0 full
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 1);
            assert_eq!(cache.free_slabs_list_occupacy_more_75.iter().count(), 0);
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
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 0);
            assert_eq!(cache.free_slabs_list_occupacy_more_75.iter().count(), 0);
            assert_eq!(cache.full_slabs_list.iter().count(), 1);

            // Alloc 0.5 slab
            let mut second_slab_ptrs = Vec::new();
            for _ in 0..cache.objects_per_slab / 2 {
                second_slab_ptrs.push(cache.alloc());
                assert!(!second_slab_ptrs.last().unwrap().is_null());
                assert!(second_slab_ptrs.last().unwrap().is_aligned());
            }

            // 1 free slabs, 1 full slabs
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 1);
            assert_eq!(cache.free_slabs_list_occupacy_more_75.iter().count(), 0);
            assert_eq!(cache.full_slabs_list.iter().count(), 1);

            // Free first slab
            first_slab_ptrs.shuffle(&mut rand::thread_rng());
            for v in first_slab_ptrs.iter() {
                cache.free(*v);
            }

            // 1 free slabs, 0 full slabs
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 1);
            assert_eq!(cache.free_slabs_list_occupacy_more_75.iter().count(), 0);
            assert_eq!(cache.full_slabs_list.iter().count(), 0);

            // Free second slab
            second_slab_ptrs.shuffle(&mut rand::thread_rng());
            for v in second_slab_ptrs.iter() {
                cache.free(*v);
            }

            // All memory free
            // 0 free slabs, 0 full slabs
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 0);
            assert_eq!(cache.free_slabs_list_occupacy_more_75.iter().count(), 0);
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
                for free_slab_info in cache
                    .free_slabs_list_occupacy_less_75
                    .iter()
                    .chain(cache.free_slabs_list_occupacy_more_75.iter())
                {
                    free_objects_counter += (*free_slab_info.data.get()).free_objects_number;
                }
                assert_eq!(cache.statistics.free_objects_number, free_objects_counter);
                assert_eq!(
                    cache.statistics.full_slabs_number,
                    cache.full_slabs_list.iter().count()
                );
                assert_eq!(
                    cache.statistics.free_slabs_number,
                    cache.free_slabs_list_occupacy_less_75.iter().count()
                        + cache.free_slabs_list_occupacy_more_75.iter().count()
                );

                // Free all objects
                allocated_ptrs.shuffle(&mut rand::thread_rng());
                for v in allocated_ptrs.into_iter() {
                    cache.free(v);
                }
                assert_eq!(cache.memory_backend.allocated_slab_addrs.len(), 0);
            }

            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 0);
            assert_eq!(cache.free_slabs_list_occupacy_more_75.iter().count(), 0);
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
            // 32 objects
            let mut cache: Cache<TestObjectType256, TestMemoryBackend> =
                Cache::new(SLAB_SIZE, PAGE_SIZE, OBJECT_SIZE_TYPE, test_memory_backend).unwrap();
            assert_eq!(cache.objects_per_slab, 32);

            // Alloc 1
            let allocated_ptr = cache.alloc();
            assert!(!allocated_ptr.is_null());
            assert!(allocated_ptr.is_aligned());
            // Free 1
            cache.free(allocated_ptr);
            assert!(cache.free_slabs_list_occupacy_less_75.is_empty());
            assert!(cache.free_slabs_list_occupacy_more_75.is_empty());
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
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 0);
            assert_eq!(cache.free_slabs_list_occupacy_more_75.iter().count(), 1);
            assert_eq!(cache.full_slabs_list.iter().count(), 0);

            // Alloc last object
            first_slab_ptrs.push(cache.alloc());
            assert!(!first_slab_ptrs.last().unwrap().is_null());
            assert!(first_slab_ptrs.last().unwrap().is_aligned());

            let first_slab_ptrs_copy = first_slab_ptrs.clone();

            // 0 free slabs, 1 full
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 0);
            assert_eq!(cache.free_slabs_list_occupacy_more_75.iter().count(), 0);
            assert_eq!(cache.full_slabs_list.iter().count(), 1);

            // Mix addresses
            first_slab_ptrs.shuffle(&mut rand::thread_rng());

            // Free all objects except one
            let len = first_slab_ptrs.len() - 1;
            for _ in 0..len {
                cache.free(first_slab_ptrs.pop().unwrap());
            }
            // 1 free slabs, 0 full
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 1);
            assert_eq!(cache.free_slabs_list_occupacy_more_75.iter().count(), 0);
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
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 0);
            assert_eq!(cache.free_slabs_list_occupacy_more_75.iter().count(), 0);
            assert_eq!(cache.full_slabs_list.iter().count(), 1);

            // Alloc 0.5 slab
            let mut second_slab_ptrs = Vec::new();
            for _ in 0..cache.objects_per_slab / 2 {
                second_slab_ptrs.push(cache.alloc());
                assert!(!second_slab_ptrs.last().unwrap().is_null());
                assert!(second_slab_ptrs.last().unwrap().is_aligned());
            }

            // 1 free slabs, 1 full slabs
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 1);
            assert_eq!(cache.free_slabs_list_occupacy_more_75.iter().count(), 0);
            assert_eq!(cache.full_slabs_list.iter().count(), 1);

            // Free first slab
            first_slab_ptrs.shuffle(&mut rand::thread_rng());
            for v in first_slab_ptrs.iter() {
                cache.free(*v);
            }

            // 1 free slabs, 0 full slabs
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 1);
            assert_eq!(cache.free_slabs_list_occupacy_more_75.iter().count(), 0);
            assert_eq!(cache.full_slabs_list.iter().count(), 0);

            // Free second slab
            second_slab_ptrs.shuffle(&mut rand::thread_rng());
            for v in second_slab_ptrs.iter() {
                cache.free(*v);
            }

            // All memory free
            // 0 free slabs, 0 full slabs
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 0);
            assert_eq!(cache.free_slabs_list_occupacy_more_75.iter().count(), 0);
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
                for free_slab_info in cache
                    .free_slabs_list_occupacy_less_75
                    .iter()
                    .chain(cache.free_slabs_list_occupacy_more_75.iter())
                {
                    free_objects_counter += (*free_slab_info.data.get()).free_objects_number;
                }
                assert_eq!(cache.statistics.free_objects_number, free_objects_counter);
                assert_eq!(
                    cache.statistics.full_slabs_number,
                    cache.full_slabs_list.iter().count()
                );
                assert_eq!(
                    cache.statistics.free_slabs_number,
                    cache.free_slabs_list_occupacy_less_75.iter().count()
                        + cache.free_slabs_list_occupacy_more_75.iter().count()
                );

                // Free all objects
                allocated_ptrs.shuffle(&mut rand::thread_rng());
                for v in allocated_ptrs.into_iter() {
                    cache.free(v);
                }
                assert_eq!(cache.memory_backend.allocated_slab_addrs.len(), 0);
            }
            assert!(cache.free_slabs_list_occupacy_less_75.is_empty());
            assert!(cache.free_slabs_list_occupacy_more_75.is_empty());
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
    #[test]
    fn slab_occupacy_lists() {
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
            // 32 objects
            // 75% is 24
            let mut cache: Cache<TestObjectType256, TestMemoryBackend> =
                Cache::new(SLAB_SIZE, PAGE_SIZE, OBJECT_SIZE_TYPE, test_memory_backend).unwrap();
            assert_eq!(cache.objects_per_slab, 32);

            assert!(cache.free_slabs_list_occupacy_less_75.is_empty());
            assert!(cache.free_slabs_list_occupacy_less_75.is_empty());

            // Alloc 23 objects
            let mut allocated_ptrs = Vec::new();
            for _ in 0..23 {
                let allocatred_ptr = cache.alloc();
                assert!(!allocatred_ptr.is_null());
                assert!(allocatred_ptr.is_aligned());
                allocated_ptrs.push(allocatred_ptr);
            }

            // 1 free slab in free (<75)
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 1);
            assert_eq!(cache.free_slabs_list_occupacy_more_75.iter().count(), 0);
            assert_eq!(cache.full_slabs_list.iter().count(), 0);

            // free (<75) -> free (>75)
            // Alloc 1 obj
            allocated_ptrs.push(cache.alloc());
            assert!(!allocated_ptrs.last().unwrap().is_null());
            assert!(allocated_ptrs.last().unwrap().is_aligned());
            // 1 free slab in free (>75) list
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 0);
            assert_eq!(cache.free_slabs_list_occupacy_more_75.iter().count(), 1);

            // free (>75) -> full
            // Alloc remaining 8 objects from slab
            for _ in 0..8 {
                let allocatred_ptr = cache.alloc();
                assert!(!allocatred_ptr.is_null());
                assert!(allocatred_ptr.is_aligned());
                allocated_ptrs.push(allocatred_ptr);
            }

            // 1 full slab
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 0);
            assert_eq!(cache.free_slabs_list_occupacy_more_75.iter().count(), 0);
            assert_eq!(cache.full_slabs_list.iter().count(), 1);
            assert_eq!(allocated_ptrs.len(), 32);

            // full -> free (>75)
            // Free 8 objects
            allocated_ptrs.shuffle(&mut thread_rng());
            for _ in 0..8 {
                let allocated_ptr = allocated_ptrs.pop().unwrap();
                cache.free(allocated_ptr);
            }
            assert_eq!(
                (*cache
                    .free_slabs_list_occupacy_more_75
                    .front()
                    .get()
                    .unwrap()
                    .data
                    .get())
                .free_objects_number,
                8
            );

            // 1 slab in free (>75)
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 0);
            assert_eq!(cache.free_slabs_list_occupacy_more_75.iter().count(), 1);
            assert_eq!(cache.full_slabs_list.iter().count(), 0);

            // free (>75) -> free (<75)
            // Free 1 object
            cache.free(allocated_ptrs.pop().unwrap());

            // 1 slab in free (<75)
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 1);
            assert_eq!(cache.free_slabs_list_occupacy_more_75.iter().count(), 0);
            assert_eq!(cache.full_slabs_list.iter().count(), 0);
            assert_eq!(
                (*cache
                    .free_slabs_list_occupacy_less_75
                    .front()
                    .get()
                    .unwrap()
                    .data
                    .get())
                .free_objects_number,
                9
            );

            // Free remain (23) objects
            assert_eq!(allocated_ptrs.len(), 23);
            for i in 0..23 {
                let allocated_ptr = allocated_ptrs[i];
                cache.free(allocated_ptr);
            }
            assert_eq!(cache.free_slabs_list_occupacy_less_75.iter().count(), 0);
            assert_eq!(cache.free_slabs_list_occupacy_more_75.iter().count(), 0);
            assert_eq!(cache.full_slabs_list.iter().count(), 0);
        }
    }
}
