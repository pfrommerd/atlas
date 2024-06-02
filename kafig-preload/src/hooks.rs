use libc::{c_char, c_int};
use libc::{DIR, FILE};

// open
redhook::hook! {
    unsafe fn open(path: *const c_char, flags: c_int, mode: c_int) -> c_int => my_open {
        println!("Opening path..");
        let real = redhook::real!(open);
        real(path, flags, mode)
    }
}

// open64
redhook::hook! {
    unsafe fn open64(path: *const c_char, flags: c_int, mode: c_int) -> c_int => my_open64 {
        println!("Open64..");
        let real = redhook::real!(open64);
        real(path, flags, mode)
    }
}

// fopen
redhook::hook! {
    unsafe fn fopen(path: *const c_char, mode: *const c_char) -> *mut FILE => my_fopen {
        let cpath = unsafe { std::ffi::CStr::from_ptr(path) };
        println!("Opening file {cpath:?}");
        let real = redhook::real!(fopen);
        real(path, mode)
    }
}

// opendir
redhook::hook! {
    unsafe fn opendir(path: *const c_char) -> *mut DIR => my_opendir {
        let cpath = unsafe { std::ffi::CStr::from_ptr(path) };
        println!("Opening directory {cpath:?}");
        let real = redhook::real!(opendir);
        real(path)
    }
}
