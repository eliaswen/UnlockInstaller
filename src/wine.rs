//! Detects the Windows build being run under Wine or Proton.

/// Whether this process runs under Wine/Proton rather than real Windows.
/// Wine's `ntdll.dll` exports `wine_get_version`; Windows' does not.
#[cfg(windows)]
pub fn detected() -> bool {
    use std::ffi::{c_char, c_void};

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleA(name: *const c_char) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    }
    // SAFETY: both arguments are valid NUL-terminated strings, and ntdll is
    // loaded in every process for its whole lifetime.
    unsafe {
        let ntdll = GetModuleHandleA(c"ntdll.dll".as_ptr());
        !ntdll.is_null() && !GetProcAddress(ntdll, c"wine_get_version".as_ptr()).is_null()
    }
}

#[cfg(not(windows))]
pub fn detected() -> bool {
    false
}

#[cfg(all(test, windows))]
mod tests {
    /// The Windows tests are run under Wine when cross-compiling from Linux
    /// (set `EXPECT_WINE=1`); on real Windows this must be false.
    #[test]
    fn detects_wine() {
        assert_eq!(super::detected(), std::env::var_os("EXPECT_WINE").is_some());
    }
}
