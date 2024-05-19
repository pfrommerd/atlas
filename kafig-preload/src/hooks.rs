use libc::{c_char, c_int};
use libc::{DIR, FILE};


// open
redhook::hook! {
    unsafe fn open(path: *const c_char, flags: c_int, mode: c_int) -> c_int => my_open {
        let real = redhook::real!(open);
        real(path, flags, mode)
    }
}

// open64
redhook::hook! {
    unsafe fn open64(path: *const c_char, flags: c_int, mode: c_int) -> c_int => my_open64 {
        let real = redhook::real!(open64);
        real(path, flags, mode)
    }
}

// fopen
redhook::hook! {
    unsafe fn fopen(path: *const c_char, mode: *const c_char) -> *mut FILE => my_fopen {
        let real = redhook::real!(fopen);
        real(path, mode)
    }
}

// opendir
redhook::hook! {
    unsafe fn opendir(path: *const c_char) -> *mut DIR => my_opendir {
        println!("Opening directory..");
        let real = redhook::real!(opendir);
        real(path)
    }
}
