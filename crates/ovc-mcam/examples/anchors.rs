//! Dev probe: print the face-anchor placement (center offset, size in face units) and image
//! size of every face sticker in the ManyCam packs — reference numbers for drawing our own.
//! Run: `cargo run -p ovc-mcam --example anchors`

fn main() -> anyhow::Result<()> {
    let dir = ovc_mcam::default_pack_dir().expect("ManyCam pack dir not found");
    for p in ovc_mcam::list_packs(&dir)? {
        for r in ovc_mcam::list_effects_in_pack(&p).unwrap_or_default() {
            if r.kind != ovc_mcam::Kind::FaceMask {
                continue;
            }
            let Ok(e) = ovc_mcam::load_effect(&r.pack_path, &r.mcv_name) else { continue };
            if e.face.as_ref().is_some_and(|f| f.pivots.len() >= 3) {
                continue; // warped mask, not an anchored sticker
            }
            let (w, h) = e.frames.first().map(|f| (f.w, f.h)).unwrap_or((0, 0));
            println!(
                "{:<18} {:<28} pos [{:6.2},{:6.2}] size [{:5.2},{:5.2}] img {}x{} frames {}",
                p.file_stem().unwrap().to_string_lossy(),
                e.name,
                e.position[0],
                e.position[1],
                e.size[0],
                e.size[1],
                w,
                h,
                e.frames.len()
            );
        }
    }
    // Downloaded category folders (.mcv files).
    if let Some(vd) = ovc_mcam::default_video_dir() {
        for cat in ovc_mcam::list_category_dirs(&vd).unwrap_or_default() {
            for r in ovc_mcam::list_effects_in_dir(&cat).unwrap_or_default() {
                if r.kind != ovc_mcam::Kind::FaceMask {
                    continue;
                }
                let Some(f) = &r.file else { continue };
                let Ok(e) = ovc_mcam::load_mcv_file(f) else { continue };
                if e.face.as_ref().is_some_and(|f| f.pivots.len() >= 3) {
                    continue;
                }
                let (w, h) = e.frames.first().map(|f| (f.w, f.h)).unwrap_or((0, 0));
                println!(
                    "{:<18} {:<28} pos [{:6.2},{:6.2}] size [{:5.2},{:5.2}] img {}x{} frames {}",
                    cat.file_name().unwrap().to_string_lossy(),
                    e.name,
                    e.position[0],
                    e.position[1],
                    e.size[0],
                    e.size[1],
                    w,
                    h,
                    e.frames.len()
                );
            }
        }
    }
    Ok(())
}
