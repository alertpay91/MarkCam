//! **MarkCam** DirectShow virtual camera — a COM in-process server (`markcam_dshow.dll`).
//!
//! Apps that enumerate DirectShow video capture devices (ClassIn, Zoom, Teams classic,
//! OBS, browsers) see a webcam named "MarkCam". The filter is loaded *into those apps*;
//! it reads OpenVCam Studio's final frames from the shared `OVC1` ring
//! (`ovc_ipc::VCAM_RING_NAME`) and serves them as NV12 / YUY2 / RGB32 at 30 fps. While
//! the Studio isn't running it serves a dark placeholder frame.
//!
//! Install: `regsvr32 markcam_dshow.dll` (elevated; writes HKLM). Uninstall: `regsvr32 /u`.
//! 64-bit apps need the 64-bit DLL (ClassIn is 64-bit).
#![cfg(windows)]
#![allow(non_snake_case)]

mod convert;
mod filter;
mod formats;
mod source;

use std::ffi::c_void;

use windows::core::{implement, Interface, Result, GUID, HRESULT, PCWSTR};
use windows::Win32::Foundation::{
    BOOL, CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_FAIL, E_POINTER, HMODULE, S_FALSE, S_OK,
};
use windows::Win32::Media::DirectShow::{
    IBaseFilter, IFilterMapper2, MERIT_DO_NOT_USE, REGFILTER2, REGFILTER2_0, REGFILTER2_0_1, REGFILTERPINS2,
    REGPINTYPES, REG_PINFLAG_B_OUTPUT,
};
use windows::Win32::Media::MediaFoundation::{
    CLSID_FilterMapper2, CLSID_VideoInputDeviceCategory, MEDIATYPE_Video, PIN_CATEGORY_CAPTURE,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, IClassFactory, IClassFactory_Impl, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::LibraryLoader::{
    GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
    GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
};
use windows::Win32::System::Registry::{RegDeleteTreeW, RegSetKeyValueW, HKEY_LOCAL_MACHINE, REG_SZ};

pub use filter::create_filter;

/// {A17D78D7-3C5C-40B8-B6AF-2A1DBD964591}
pub const CLSID_OPENVCAM: GUID = GUID::from_u128(0xa17d78d7_3c5c_40b8_b6af_2a1dbd964591);
const CLSID_STR: &str = "{A17D78D7-3C5C-40B8-B6AF-2A1DBD964591}";
/// The name apps show in their camera list.
pub const FRIENDLY_NAME: &str = "MarkCam";

#[implement(IClassFactory)]
struct Factory;

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(&self, outer: Option<&windows::core::IUnknown>, riid: *const GUID, ppv: *mut *mut c_void) -> Result<()> {
        if ppv.is_null() || riid.is_null() {
            return Err(E_POINTER.into());
        }
        unsafe { *ppv = std::ptr::null_mut() };
        if outer.is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let filter = create_filter();
        unsafe { filter.query(riid, ppv) }.ok()
    }

    fn LockServer(&self, _lock: BOOL) -> Result<()> {
        Ok(())
    }
}

/// # Safety
/// COM entry point; called by the COM runtime with valid out-pointers.
#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(rclsid: *const GUID, riid: *const GUID, ppv: *mut *mut c_void) -> HRESULT {
    if rclsid.is_null() || riid.is_null() || ppv.is_null() {
        return E_POINTER;
    }
    *ppv = std::ptr::null_mut();
    if *rclsid != CLSID_OPENVCAM {
        return CLASS_E_CLASSNOTAVAILABLE;
    }
    let factory: IClassFactory = Factory.into();
    factory.query(riid, ppv)
}

/// Never unload: streaming threads may still be winding down, and the DLL is tiny.
#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    S_FALSE
}

fn wz(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Full path of this DLL.
fn module_path() -> Result<Vec<u16>> {
    let mut hmod = HMODULE::default();
    unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            PCWSTR(DllGetClassObject as *const u16),
            &mut hmod,
        )?;
        let mut buf = vec![0u16; 1024];
        let n = GetModuleFileNameW(hmod, &mut buf) as usize;
        if n == 0 || n >= buf.len() {
            return Err(E_FAIL.into());
        }
        buf.truncate(n);
        buf.push(0);
        Ok(buf)
    }
}

fn set_sz(subkey: &str, name: Option<&str>, value: &[u16]) -> Result<()> {
    let subkey = wz(subkey);
    let name = name.map(wz);
    unsafe {
        RegSetKeyValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey.as_ptr()),
            name.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr())),
            REG_SZ.0,
            Some(value.as_ptr() as *const c_void),
            (value.len() * 2) as u32,
        )
        .ok()
    }
}

/// Run `f` with COM initialized on this thread (regsvr32 usually has it already).
fn with_com<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    let inited = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
    let r = f();
    if inited {
        unsafe { CoUninitialize() };
    }
    r
}

/// Register the COM class and list it under "Video Capture Sources".
#[no_mangle]
pub extern "system" fn DllRegisterServer() -> HRESULT {
    let r = (|| -> Result<()> {
        let clsid_key = format!("SOFTWARE\\Classes\\CLSID\\{CLSID_STR}");
        set_sz(&clsid_key, None, &wz(FRIENDLY_NAME))?;
        let inproc = format!("{clsid_key}\\InprocServer32");
        set_sz(&inproc, None, &module_path()?)?;
        set_sz(&inproc, Some("ThreadingModel"), &wz("Both"))?;

        with_com(|| unsafe {
            let mapper: IFilterMapper2 = CoCreateInstance(&CLSID_FilterMapper2, None, CLSCTX_INPROC_SERVER)?;
            let null_sub = GUID::zeroed();
            let types = REGPINTYPES { clsMajorType: &MEDIATYPE_Video, clsMinorType: &null_sub };
            let pin = REGFILTERPINS2 {
                dwFlags: REG_PINFLAG_B_OUTPUT.0 as u32,
                cInstances: 1,
                nMediaTypes: 1,
                lpMediaType: &types,
                nMediums: 0,
                lpMedium: std::ptr::null(),
                clsPinCategory: &PIN_CATEGORY_CAPTURE,
            };
            let rf2 = REGFILTER2 {
                dwVersion: 2,
                dwMerit: MERIT_DO_NOT_USE.0 as u32,
                Anonymous: REGFILTER2_0 { Anonymous2: REGFILTER2_0_1 { cPins2: 1, rgPins2: &pin } },
            };
            let name = wz(FRIENDLY_NAME);
            mapper.RegisterFilter(
                &CLSID_OPENVCAM,
                PCWSTR(name.as_ptr()),
                None,
                &CLSID_VideoInputDeviceCategory,
                PCWSTR::null(),
                &rf2,
            )
        })
    })();
    match r {
        Ok(()) => S_OK,
        Err(e) => e.code(),
    }
}

#[no_mangle]
pub extern "system" fn DllUnregisterServer() -> HRESULT {
    let _ = with_com(|| unsafe {
        let mapper: IFilterMapper2 = CoCreateInstance(&CLSID_FilterMapper2, None, CLSCTX_INPROC_SERVER)?;
        mapper.UnregisterFilter(&CLSID_VideoInputDeviceCategory, PCWSTR::null(), &CLSID_OPENVCAM)
    });
    let key = wz(&format!("SOFTWARE\\Classes\\CLSID\\{CLSID_STR}"));
    let _ = unsafe { RegDeleteTreeW(HKEY_LOCAL_MACHINE, PCWSTR(key.as_ptr())) };
    S_OK
}

/// Keeps `IBaseFilter` in the public API for the probe example.
pub type Filter = IBaseFilter;
