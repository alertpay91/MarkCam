//! Checks the *registered* filter the way a real app reaches it: enumerate "Video Capture
//! Sources", bind the OpenVCam moniker (COM loads the installed DLL), let the system
//! Capture Graph Builder connect it to a Null Renderer, and run the graph for 3 seconds.
//!
//!     cargo run -p ovc-dshow --example devenum

#[cfg(windows)]
fn main() -> windows::core::Result<()> {
    use windows::core::{Interface, GUID};
    use windows::Win32::Media::DirectShow::*;
    use windows::Win32::Media::MediaFoundation::{CLSID_VideoInputDeviceCategory, MEDIATYPE_Video, PIN_CATEGORY_CAPTURE};
    use windows::Win32::System::Com::*;

    const CLSID_SYSTEM_DEVICE_ENUM: GUID = GUID::from_u128(0x62be5d10_60eb_11d0_bd3b_00a0c911ce86);
    const CLSID_FILTER_GRAPH: GUID = GUID::from_u128(0xe436ebb3_524f_11ce_9f53_0020af0ba770);
    const CLSID_CAPTURE_GRAPH_BUILDER2: GUID = GUID::from_u128(0xbf87b6e1_8c27_11d0_b3f0_00aa003761c5);
    const CLSID_NULL_RENDERER: GUID = GUID::from_u128(0xc1f400a4_3f08_11d3_9f0b_006008039e37);

    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let devenum: ICreateDevEnum = CoCreateInstance(&CLSID_SYSTEM_DEVICE_ENUM, None, CLSCTX_INPROC_SERVER)?;
        let mut em: Option<IEnumMoniker> = None;
        devenum.CreateClassEnumerator(&CLSID_VideoInputDeviceCategory, &mut em, 0)?;
        let em = em.expect("no video capture devices");

        let mut found: Option<IBaseFilter> = None;
        let mut count = 0;
        loop {
            let mut m: [Option<IMoniker>; 1] = [None];
            if em.Next(&mut m, None).is_err() || m[0].is_none() {
                break;
            }
            count += 1;
            let m = m[0].take().unwrap();
            let Ok(f) = m.BindToObject::<_, _, IBaseFilter>(None::<&IBindCtx>, None::<&IMoniker>) else { continue };
            if f.GetClassID().ok() == Some(markcam_dshow::CLSID_OPENVCAM) {
                found = Some(f);
            }
        }
        println!("video capture devices enumerated: {count}");
        let filter = found.expect("OpenVCam not found among capture devices — is it registered?");
        println!("OpenVCam bound via device enumerator (installed DLL loaded)");

        let graph: IGraphBuilder = CoCreateInstance(&CLSID_FILTER_GRAPH, None, CLSCTX_INPROC_SERVER)?;
        let cgb: ICaptureGraphBuilder2 = CoCreateInstance(&CLSID_CAPTURE_GRAPH_BUILDER2, None, CLSCTX_INPROC_SERVER)?;
        cgb.SetFiltergraph(&graph)?;
        let name: Vec<u16> = "OpenVCam\0".encode_utf16().collect();
        graph.AddFilter(&filter, windows::core::PCWSTR(name.as_ptr()))?;
        let null: IBaseFilter = CoCreateInstance(&CLSID_NULL_RENDERER, None, CLSCTX_INPROC_SERVER)?;
        let nname: Vec<u16> = "Null\0".encode_utf16().collect();
        graph.AddFilter(&null, windows::core::PCWSTR(nname.as_ptr()))?;
        cgb.RenderStream(Some(&PIN_CATEGORY_CAPTURE), &MEDIATYPE_Video, &filter, None, &null)?;
        println!("graph built: OpenVCam -> Null Renderer");

        let control: IMediaControl = graph.cast()?;
        control.Run()?;
        std::thread::sleep(std::time::Duration::from_secs(3));
        let state = control.GetState(1000)?;
        println!("graph state after 3 s: {} (2 = running)", state);
        control.Stop()?;
        println!("DEVENUM OK");
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {}
