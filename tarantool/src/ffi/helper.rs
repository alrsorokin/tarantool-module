use dlopen::symbor::Library;

use std::ffi::CStr;
use std::os::raw::c_char;
use std::ptr::NonNull;

////////////////////////////////////////////////////////////////////////////////
// c_str!
////////////////////////////////////////////////////////////////////////////////

/// Returns a [`std::ffi::CStr`] constructed from the provided literal with a
/// nul byte appended to the end. Use this when you need a `&CStr` from a
/// string literal.
///
/// # Example
/// ```rust
/// # use tarantool::c_str;
///
/// let c_str = c_str!("hello");
/// assert_eq!(c_str.to_bytes(), b"hello");
/// ```
///
/// This macro will also check at compile time if the string literal contains
/// any interior nul bytes.
/// ```compile_fail
/// # use tarantool::c_str;
/// let this_doesnt_compile = c_str!("interior\0nul\0byte");
/// ```
#[macro_export]
macro_rules! c_str {
    ($s:expr) => {{
        #[allow(unused_unsafe)]
        const RESULT: &'static ::std::ffi::CStr = unsafe {
            ::std::ffi::CStr::from_bytes_with_nul_unchecked(::std::concat!($s, "\0").as_bytes())
        };
        RESULT
    }};
}

////////////////////////////////////////////////////////////////////////////////
// c_ptr!
////////////////////////////////////////////////////////////////////////////////

/// Returns a `*const std::ffi::c_char` constructed from the provided literal
/// with a nul byte appended to the end. Use this to pass static c-strings when
/// working with ffi.
///
/// # Example
/// ```rust
/// # use tarantool::c_ptr;
/// extern "C" {
///     fn strlen(s: *const std::ffi::c_char) -> usize;
/// }
///
/// let count = unsafe { strlen(c_ptr!("foo bar")) };
/// assert_eq!(count, 7);
/// ```
///
/// Same as [`c_str!`], this macro will check at compile time if the string
/// literal contains any interior nul bytes.
///
/// [`c_str!`]: crate::c_str
#[macro_export]
macro_rules! c_ptr {
    ($s:expr) => {
        $crate::c_str!($s).as_ptr()
    };
}

////////////////////////////////////////////////////////////////////////////////
// static_assert!
////////////////////////////////////////////////////////////////////////////////

#[macro_export]
macro_rules! static_assert {
    ($e:expr $(,)?) => {
        const _: () = assert!($e);
    };
    ($e:expr, $msg:expr $(,)?) => {
        const _: () = assert!($e, $msg);
    };
}

////////////////////////////////////////////////////////////////////////////////
// offset_of!
////////////////////////////////////////////////////////////////////////////////

/// Returns an offset of the struct or tuple member in bytes.
///
/// Returns a constant which can be used at compile time.
///
/// # Example
/// ```rust
/// # use tarantool::offset_of;
/// #[repr(C)]
/// struct MyStruct { a: u8, b: u8 }
/// assert_eq!(offset_of!(MyStruct, a), 0);
/// assert_eq!(offset_of!(MyStruct, b), 1);
///
/// // Also works with tuples:
/// assert_eq!(offset_of!((i32, i32), 0), 0);
/// assert_eq!(offset_of!((i32, i32), 1), 4);
/// ```
#[macro_export]
macro_rules! offset_of {
    ($type:ty, $field:tt) => {{
        const RESULT: usize = unsafe {
            let dummy = ::core::mem::MaybeUninit::<$type>::uninit();
            let dummy_ptr = dummy.as_ptr();
            let field_ptr = ::std::ptr::addr_of!((*dummy_ptr).$field);

            let field_ptr = field_ptr.cast::<u8>();
            let dummy_ptr = dummy_ptr.cast::<u8>();
            field_ptr.offset_from(dummy_ptr) as usize
        };
        RESULT
    }};
}

/// Returns size of type, or type's field.
///
/// Returns a constant which can be used at compile time.
///
/// # Example
/// ```rust
/// # use tarantool::size_of;
/// #[repr(C)]
/// struct MyStruct { a: u8, b: u16 }
/// assert_eq!(size_of!(MyStruct, a), 1);
/// assert_eq!(size_of!(MyStruct, b), 2);
///
/// // Also works with tuples:
/// assert_eq!(size_of!((i32, i32), 0), 4);
/// assert_eq!(size_of!((i32, i32), 1), 4);
/// ```
#[macro_export]
macro_rules! size_of {
    ($type:ty) => {
        ::std::mem::size_of::<$type>()
    };
    ($type:ty, $field:tt) => {{
        const RESULT: usize = unsafe {
            let dummy = ::core::mem::MaybeUninit::<$type>::uninit();
            let dummy_ptr = dummy.as_ptr();
            let field_ptr = ::std::ptr::addr_of!((*dummy_ptr).$field);

            const fn size_of_val<T>(_: *const T) -> usize {
                ::std::mem::size_of::<T>()
            }

            size_of_val(field_ptr)
        };
        RESULT
    }};
}

const _TEST_OFFSET_AND_SIZE_OF: () = {
    #[repr(C)]
    struct MyStruct {
        a: u8,
        b: u16,
        c: u32,
        d: u64,
    }

    assert!(offset_of!(MyStruct, a) == 0);
    assert!(offset_of!(MyStruct, b) == 2);
    assert!(offset_of!(MyStruct, c) == 4);
    assert!(offset_of!(MyStruct, d) == 8);

    assert!(size_of!(MyStruct, a) == 1);
    assert!(size_of!(MyStruct, b) == 2);
    assert!(size_of!(MyStruct, c) == 4);
    assert!(size_of!(MyStruct, d) == 8);

    assert!(offset_of!((i32, i32), 0) == 0);
    assert!(offset_of!((i32, i32), 1) == 4);

    assert!(size_of!((i32, i32), 0) == 4);
    assert!(size_of!((i32, i32), 1) == 4);
};

////////////////////////////////////////////////////////////////////////////////
// define_dlsym_reloc!
////////////////////////////////////////////////////////////////////////////////

#[macro_export]
macro_rules! define_dlsym_reloc {
    (
        $(
            $(#[$meta:meta])*
            pub fn $sym:ident ( $( $args:ident: $types:ty ),* $(,)? ) $( -> $ret:ty )?;
        )+
    ) => {
        $(
            $(#[$meta])*
            #[inline(always)]
            pub unsafe fn $sym($($args: $types),*) $(-> $ret)? {
                return RELOC_FN($($args),*);

                type SymType = unsafe fn($($args: $types),*) $(-> $ret)?;
                static mut RELOC_FN: SymType = init;

                unsafe fn init($($args: $types),*) $(-> $ret)? {
                    let sym_name = $crate::c_str!(::std::stringify!($sym));
                    let impl_fn: SymType = $crate::ffi::helper::get_any_symbol(sym_name)
                        .unwrap();
                    RELOC_FN = impl_fn;
                    RELOC_FN($($args),*)
                }
            }
        )+
    };
}

/// Find a symbol using the `tnt_internal_symbol` api.
///
/// This function performs a slow search over all the exported internal
/// tarantool symbols, so don't use it everytime you want to call a given
/// function.
#[inline]
pub unsafe fn tnt_internal_symbol<T>(name: &CStr) -> Option<T> {
    if std::mem::size_of::<T>() != std::mem::size_of::<*mut ()>() {
        return None;
    }
    let ptr = (RELOC_FN?)(name.as_ptr())?;
    return Some(std::mem::transmute_copy(&ptr));

    type SymType = unsafe fn(*const c_char) -> Option<NonNull<()>>;
    static mut RELOC_FN: Option<SymType> = Some(init);

    unsafe fn init(name: *const c_char) -> Option<NonNull<()>> {
        let lib = Library::open_self().ok()?;
        match lib.symbol_cstr(c_str!("tnt_internal_symbol")) {
            Ok(sym) => {
                RELOC_FN = Some(*sym);
                (RELOC_FN.unwrap())(name)
            }
            Err(_) => {
                RELOC_FN = None;
                None
            }
        }
    }
}

/// Check if symbol can be found in the current executable using dlsym.
#[inline]
pub unsafe fn has_dyn_symbol(name: &CStr) -> bool {
    get_dyn_symbol::<*const ()>(name).is_ok()
}

/// Find a sybmol in the current executable using dlsym.
#[inline]
pub unsafe fn get_dyn_symbol<T: Copy>(name: &CStr) -> Result<T, dlopen::Error> {
    let lib = Library::open_self()?;
    let sym = lib.symbol_cstr(name)?;
    Ok(*sym)
}

/// Find a symbol either using the `tnt_internal_symbol` api or using dlsym as a
/// fallback.
#[inline]
pub unsafe fn get_any_symbol<T: Copy>(name: &CStr) -> Result<T, dlopen::Error> {
    if let Some(sym) = tnt_internal_symbol(name) {
        return Ok(sym);
    }
    let lib = Library::open_self()?;
    let sym = lib.symbol_cstr(name)?;
    Ok(*sym)
}

////////////////////////////////////////////////////////////////////////////////
// pointer stuff
////////////////////////////////////////////////////////////////////////////////

/// Returns `true` if `p` points into a mapped memory page.
#[inline(always)]
#[cfg(unix)]
pub unsafe fn pointer_is_in_mapped_pages<T>(p: *const T) -> bool {
    const NUM_PAGES: usize = 1;

    let page_size = get_page_size();
    let page_start = align_to(p, page_size);
    let mut v = [0_u8; NUM_PAGES];

    let rc = libc::mincore(
        page_start as _,
        (NUM_PAGES * page_size as usize) as _,
        v.as_mut_ptr() as _,
    );
    if rc != 0 {
        let e = std::io::Error::last_os_error().raw_os_error().unwrap();
        // FIXME: this could also be a EAGAIN
        debug_assert_eq!(e, libc::ENOMEM);
        return false;
    }

    return true;
}

/// Returns the memory page size on the current system.
#[inline(always)]
pub fn get_page_size() -> u64 {
    use std::sync::OnceLock;
    static mut ONCE: OnceLock<u64> = OnceLock::new();
    // Safety: docs say OnceLock::get_or_init is safe to be called from
    // different threads.
    unsafe {
        *ONCE.get_or_init(|| {
            #[cfg(unix)]
            let page_size = libc::sysconf(libc::_SC_PAGESIZE) as _;

            #[cfg(not(unix))]
            let page_size = 4096;

            page_size
        })
    }
}

/// Returns the memory address aligned to the given `alignment`.
///
/// You can use this to for example to compute the start address of the page `p`
/// points into:
/// ```rust
/// use tarantool::ffi::helper::{align_to, get_page_size};
///
/// let arr: [u8; 2] = [1, 2];
/// let p0 = &arr[0] as *const u8;
/// let p1 = &arr[1] as *const u8;
///
/// let p0_page = align_to(p0, get_page_size()).cast::<u8>();
/// let p1_page = align_to(p1, get_page_size()).cast::<u8>();
/// assert_eq!(p0_page, p1_page);
/// assert!(p0_page <= p0);
/// assert!(p1_page < p1);
/// ```
#[inline(always)]
pub fn align_to<T>(p: *const T, alignment: u64) -> *const () {
    debug_assert!(alignment.is_power_of_two(), "doesn't make sense otherwise");
    return ((p as u64) & !(alignment - 1)) as _;
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn align_to_page() {
        #[repr(align(8))]
        struct S {
            bytes: [u8; 8],
        }
        let s = S { bytes: [0; 8] };

        let p = &s.bytes[0] as *const u8 as *const ();
        let q = &s.bytes[7] as *const u8 as *const ();

        let page_size = get_page_size();

        let p_page = align_to(p, page_size);
        assert!(p_page <= p);

        let q_page = align_to(q, page_size);
        assert!(q_page < q);

        assert_eq!(p_page, q_page);

        // pointer inside a struct aligned to the struct's alignment is the
        // start of the struct (not always, but you get me...)
        assert_eq!(p, align_to(q, std::mem::align_of::<S>() as _));
    }
}
