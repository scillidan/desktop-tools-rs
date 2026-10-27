//! Native virtual desktops (Windows 10 1809-22H2 and Windows 11).
//!
//! Windows has no public API for switching desktops. This uses the
//! undocumented `IVirtualDesktopManagerInternal` interface to switch, create
//! and move windows. Windows 11 keeps the same vtable but uses different IIDs,
//! so both known interface pairs are queried without OS-build detection.

use anyhow::{bail, Context, Result};
use std::ffi::c_void;
use std::ptr::null_mut;
use windows::core::{IUnknown, Interface, GUID, HRESULT};
use windows::Win32::Foundation::{HWND, RPC_E_CHANGED_MODE};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED,
};

const CLSID_IMMERSIVE_SHELL: GUID = GUID::from_u128(0xC2F03A33_21F5_47FA_B4BB_156362A2F239);
const IID_SERVICE_PROVIDER: GUID = GUID::from_u128(0x6D5140C1_7436_11CE_8034_00AA006009FA);
const CLSID_VDESK_INTERNAL: GUID = GUID::from_u128(0xC5E0CDCA_7B6E_41B2_9FC4_D93975CC467B);
const IID_VDESK_INTERNAL_WIN10: GUID = GUID::from_u128(0xF31574D6_B682_4CDC_BD56_1827860ABEC6);
const IID_VDESK_INTERNAL_WIN11: GUID = GUID::from_u128(0x53F5CA0B_158F_4124_900C_057158060B27);
const IID_VIRTUAL_DESKTOP_WIN10: GUID = GUID::from_u128(0xFF72FFDD_BE7E_43FC_9C03_AD81681E88E4);
const IID_VIRTUAL_DESKTOP_WIN11: GUID = GUID::from_u128(0x3F07F4BE_B107_441A_AF0F_39D82529072C);
const IID_VIEW_COLLECTION: GUID = GUID::from_u128(0x1841C6D7_4F9D_42C0_AF41_8747538F10E5);

#[repr(C)]
struct UnknownVtbl {
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
}

#[repr(C)]
struct ServiceProviderVtbl {
    query_interface: usize,
    add_ref: usize,
    release: usize,
    query_service: unsafe extern "system" fn(
        *mut c_void,
        *const GUID,
        *const GUID,
        *mut *mut c_void,
    ) -> HRESULT,
}

/// Windows 10/11 layout. Unused slots stay pointer-sized placeholders.
#[repr(C)]
struct InternalVtbl {
    query_interface: usize,
    add_ref: usize,
    release: usize,
    get_count: unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT,
    move_view_to_desktop:
        unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> HRESULT,
    can_view_move_desktops: usize,
    get_current_desktop: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
    get_desktops: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
    get_adjacent_desktop: usize,
    switch_desktop: unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT,
    switch_desktop_and_move_foreground_view: usize,
    create_desktop: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
}

#[repr(C)]
struct ObjectArrayVtbl {
    query_interface: usize,
    add_ref: usize,
    release: usize,
    get_count: unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT,
    get_at: unsafe extern "system" fn(*mut c_void, u32, *const GUID, *mut *mut c_void) -> HRESULT,
}

/// IVirtualDesktop vtable (only GetId is used, by `current_index`).
#[repr(C)]
struct VirtualDesktopVtbl {
    query_interface: usize,
    add_ref: usize,
    release: usize,
    is_view_visible: usize,
    get_id: unsafe extern "system" fn(*mut c_void, *mut GUID) -> HRESULT,
}

#[repr(C)]
struct ViewCollectionVtbl {
    query_interface: usize,
    add_ref: usize,
    release: usize,
    get_views: usize,
    get_views_by_zorder: usize,
    get_views_by_app_user_model_id: usize,
    get_view_for_hwnd: unsafe extern "system" fn(*mut c_void, HWND, *mut *mut c_void) -> HRESULT,
}

/// Owned COM interface pointer; releases on drop, including error paths.
struct Com(*mut c_void);

impl Com {
    fn raw(&self) -> *mut c_void {
        self.0
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                ((*vtbl::<UnknownVtbl>(self.0)).release)(self.0);
            }
        }
    }
}

unsafe fn vtbl<T>(this: *mut c_void) -> *const T {
    unsafe { *(this as *const *const T) }
}

fn check(hr: HRESULT, what: &str) -> Result<()> {
    hr.ok().with_context(|| what.to_string())
}

pub(crate) unsafe fn com_init() -> Result<()> {
    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    // S_FALSE means the thread already joined this apartment; a different
    // apartment (MTA) still works with the out-of-process shell server.
    if hr.is_err() && hr != RPC_E_CHANGED_MODE {
        bail!("CoInitializeEx failed: {hr:?}");
    }
    Ok(())
}

unsafe fn query_interface(obj: *mut c_void, iid: &GUID) -> Result<Com> {
    let mut out = null_mut();
    let hr = unsafe { ((*vtbl::<UnknownVtbl>(obj)).query_interface)(obj, iid, &mut out) };
    check(hr, "QueryInterface failed")?;
    Ok(Com(out))
}

/// The shell's `IServiceProvider`, the gateway to the desktop interfaces.
unsafe fn shell_provider() -> Result<Com> {
    unsafe {
        com_init()?;
        let shell: IUnknown = CoCreateInstance(
            &CLSID_IMMERSIVE_SHELL,
            None::<&IUnknown>,
            CLSCTX_LOCAL_SERVER,
        )
        .context("cannot create the ImmersiveShell object")?;
        query_interface(shell.as_raw(), &IID_SERVICE_PROVIDER)
            .context("IServiceProvider is unavailable")
    }
}

unsafe fn query_service(provider: &Com, service: &GUID, iid: &GUID) -> Result<Com> {
    let mut out = null_mut();
    let hr = unsafe {
        ((*vtbl::<ServiceProviderVtbl>(provider.raw())).query_service)(
            provider.raw(),
            service,
            iid,
            &mut out,
        )
    };
    check(hr, "QueryService failed")?;
    Ok(Com(out))
}

/// Create the shell's `IVirtualDesktopManagerInternal`.
unsafe fn manager_internal() -> Result<Com> {
    let provider = unsafe { shell_provider() }?;
    for iid in [IID_VDESK_INTERNAL_WIN10, IID_VDESK_INTERNAL_WIN11] {
        if let Ok(internal) = unsafe { query_service(&provider, &CLSID_VDESK_INTERNAL, &iid) } {
            return Ok(internal);
        }
    }
    bail!("IVirtualDesktopManagerInternal is unavailable (Windows 10 and 11 IIDs failed)")
}

/// `IVirtualDesktop` also changed IID on Windows 11; the array vtable did not.
unsafe fn desktop_at(array: &Com, index: u32) -> Result<Com> {
    for iid in [IID_VIRTUAL_DESKTOP_WIN10, IID_VIRTUAL_DESKTOP_WIN11] {
        let mut desktop = null_mut();
        let hr = unsafe {
            ((*vtbl::<ObjectArrayVtbl>(array.raw())).get_at)(array.raw(), index, &iid, &mut desktop)
        };
        if hr.is_ok() {
            return Ok(Com(desktop));
        }
    }
    bail!(
        "cannot pick desktop {}; unsupported IVirtualDesktop IID",
        index + 1
    )
}

/// Create desktops until `n` exist, so numbered bindings work before the user
/// has made that many in Task View.
unsafe fn ensure_desktops(internal: *mut c_void, n: u8) -> Result<()> {
    unsafe {
        for _ in 0..n {
            let mut count = 0u32;
            check(
                ((*vtbl::<InternalVtbl>(internal)).get_count)(internal, &mut count),
                "GetCount failed",
            )?;
            if count >= u32::from(n) {
                return Ok(());
            }
            let mut created = null_mut();
            check(
                ((*vtbl::<InternalVtbl>(internal)).create_desktop)(internal, &mut created),
                "CreateDesktop failed",
            )?;
            if !created.is_null() {
                ((*vtbl::<UnknownVtbl>(created)).release)(created);
            }
        }
        bail!("could not create enough desktops for desktop {n}")
    }
}

/// 1-based desktop index, creating desktops up to `n` if needed.
unsafe fn nth_desktop(internal: *mut c_void, n: u8) -> Result<Com> {
    if n == 0 {
        bail!("desktop number must be 1..=9");
    }
    unsafe { ensure_desktops(internal, n) }?;
    unsafe {
        let mut array = null_mut();
        check(
            ((*vtbl::<InternalVtbl>(internal)).get_desktops)(internal, &mut array),
            "GetDesktops failed",
        )?;
        let array = Com(array);
        let mut count = 0u32;
        check(
            ((*vtbl::<ObjectArrayVtbl>(array.raw())).get_count)(array.raw(), &mut count),
            "cannot read the desktop count",
        )?;
        if u32::from(n) > count {
            bail!("desktop {n} does not exist (only {count} desktops)");
        }
        desktop_at(&array, u32::from(n) - 1)
    }
}

/// Switch to desktop `n` (1-based), creating desktops up to `n` if needed.
pub fn switch_to(n: u8) -> Result<()> {
    unsafe {
        let internal = manager_internal()?;
        let desktop = nth_desktop(internal.raw(), n)?;
        check(
            ((*vtbl::<InternalVtbl>(internal.raw())).switch_desktop)(internal.raw(), desktop.raw()),
            "SwitchDesktop failed",
        )
    }
}

/// Move a top-level window to desktop `n` (1-based). Does not switch.
pub fn move_window_to(hwnd: HWND, n: u8) -> Result<()> {
    unsafe {
        let internal = manager_internal()?;
        let desktop = nth_desktop(internal.raw(), n)?;
        let provider = shell_provider()?;
        let views = query_service(&provider, &IID_VIEW_COLLECTION, &IID_VIEW_COLLECTION)
            .context("IApplicationViewCollection is unavailable")?;
        let mut view = null_mut();
        check(
            ((*vtbl::<ViewCollectionVtbl>(views.raw())).get_view_for_hwnd)(
                views.raw(),
                hwnd,
                &mut view,
            ),
            "cannot get the window's application view",
        )?;
        if view.is_null() {
            bail!("the window has no application view");
        }
        let view = Com(view);
        check(
            ((*vtbl::<InternalVtbl>(internal.raw())).move_view_to_desktop)(
                internal.raw(),
                view.raw(),
                desktop.raw(),
            ),
            "MoveViewToDesktop failed",
        )
    }
}

/// Number of existing desktops.
///
/// # Safety
/// `internal` must be a valid `IVirtualDesktopManagerInternal`.
unsafe fn desktop_count(internal: *mut c_void) -> Result<u32> {
    let mut count = 0u32;
    check(
        ((*vtbl::<InternalVtbl>(internal)).get_count)(internal, &mut count),
        "GetCount failed",
    )?;
    Ok(count)
}

/// 1-based index of the current desktop, resolved through the same objects.
///
/// # Safety
/// `internal` must be a valid `IVirtualDesktopManagerInternal`.
unsafe fn current_index(internal: *mut c_void) -> Result<u32> {
    let mut current = null_mut();
    check(
        ((*vtbl::<InternalVtbl>(internal)).get_current_desktop)(internal, &mut current),
        "GetCurrentDesktop failed",
    )?;
    let current = Com(current);
    let mut current_id = GUID::default();
    check(
        ((*vtbl::<VirtualDesktopVtbl>(current.raw())).get_id)(current.raw(), &mut current_id),
        "GetId failed",
    )?;

    let mut array = null_mut();
    check(
        ((*vtbl::<InternalVtbl>(internal)).get_desktops)(internal, &mut array),
        "GetDesktops failed",
    )?;
    let array = Com(array);
    let count = desktop_count(internal)?;
    for i in 0..count {
        let desktop = unsafe { desktop_at(&array, i) }.context("GetAt failed")?;
        let mut id = GUID::default();
        check(
            ((*vtbl::<VirtualDesktopVtbl>(desktop.raw())).get_id)(desktop.raw(), &mut id),
            "GetId failed",
        )?;
        if id == current_id {
            return Ok(i + 1);
        }
    }
    bail!("the current desktop is missing from GetDesktops")
}

/// Switch `delta` desktops away (-1/1), wrapping around the ends. Existing
/// desktops only; never creates one.
pub fn switch_relative(delta: i32) -> Result<()> {
    unsafe {
        let internal = manager_internal()?;
        let count = desktop_count(internal.raw())? as i32;
        if count <= 1 {
            return Ok(());
        }
        let cur = current_index(internal.raw())? as i32;
        let target = (cur - 1 + delta).rem_euclid(count) + 1;
        let desktop = nth_desktop(internal.raw(), target as u8)?;
        check(
            ((*vtbl::<InternalVtbl>(internal.raw())).switch_desktop)(internal.raw(), desktop.raw()),
            "SwitchDesktop failed",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_vtable_slots_match_the_com_interface() {
        assert_eq!(
            std::mem::offset_of!(InternalVtbl, create_desktop),
            11 * std::mem::size_of::<usize>()
        );
    }

    #[test]
    fn reads_the_desktop_count() {
        unsafe {
            let internal = manager_internal().expect("Windows 10/11 virtual desktop API");
            let count = desktop_count(internal.raw()).expect("GetCount");
            assert!(count >= 1);
            assert!(current_index(internal.raw()).is_ok());
        }
    }

    #[test]
    #[ignore = "switches the real desktop twice; run with --ignored"]
    fn switches_to_another_desktop_and_back() {
        unsafe {
            let internal = manager_internal().expect("Windows 10/11 virtual desktop API");
            ensure_desktops(internal.raw(), 2).expect("ensure two desktops");
            let start = current_index(internal.raw()).expect("current desktop") as u8;
            let target: u8 = if start == 1 { 2 } else { 1 };
            switch_to(target).expect("switch");
            assert_eq!(
                current_index(internal.raw()).expect("current"),
                u32::from(target)
            );
            switch_to(start).expect("switch back");
            assert_eq!(
                current_index(internal.raw()).expect("current"),
                u32::from(start)
            );
        }
    }

    #[test]
    #[ignore = "moves a temporary real window between desktops; run with --ignored"]
    fn moves_a_window_to_another_desktop_and_back() {
        use windows::core::w;
        use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, CW_USEDEFAULT,
            WINDOW_EX_STYLE, WNDCLASSW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
        };

        unsafe extern "system" fn wnd_proc(
            hwnd: HWND,
            msg: u32,
            wparam: WPARAM,
            lparam: LPARAM,
        ) -> LRESULT {
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }

        unsafe {
            let internal = manager_internal().expect("Windows 10/11 virtual desktop API");
            ensure_desktops(internal.raw(), 2).expect("ensure two desktops");
            let start = current_index(internal.raw()).expect("current desktop") as u8;
            let target: u8 = if start == 1 { 2 } else { 1 };

            let instance = HINSTANCE::default();
            let class = w!("desktop-tools-vdesk-test");
            let wc = WNDCLASSW {
                hInstance: instance,
                lpszClassName: class,
                lpfnWndProc: Some(wnd_proc),
                ..Default::default()
            };
            assert_ne!(RegisterClassW(&wc), 0);
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class,
                w!("desktop-tools virtual desktop test"),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                400,
                300,
                None,
                None,
                Some(instance),
                None,
            )
            .expect("create temporary window");
            std::thread::sleep(std::time::Duration::from_millis(100));

            let moved = move_window_to(hwnd, target);
            let back = move_window_to(hwnd, start);
            let _ = DestroyWindow(hwnd);
            moved.expect("move the temporary window");
            back.expect("move it back to the start desktop");
        }
    }
}
