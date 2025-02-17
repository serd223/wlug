#[no_mangle]
pub extern "C" fn __name() -> *const u8 {
    b"plug4\0".as_ptr()
}

extern "C" {
    fn print(a: i32);
    fn plug2(a: i32);
}

#[no_mangle]
pub extern "C" fn __deps() -> *const u8 {
    b"plug2\0".as_ptr()
}

#[no_mangle]
pub unsafe extern "C" fn plug4(a: i32) {
    plug2(a + 20);
    mul(a, a);
}

// Even though plug2, which is imported by this plugin, also has a
// `mul` function, there aren't any name collisions thanks to the linker.
#[no_mangle]
pub unsafe extern "C" fn mul(a: i32, b: i32) -> i32 {
    let res = a * b + 10;
    print(res);
    res
}
