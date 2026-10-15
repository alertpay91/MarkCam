//! Dev probe: list ManyCam packs/effects and decode one effect.
//! Run: `cargo run -p ovc-mcam --example dump`

fn main() -> anyhow::Result<()> {
    let dir = ovc_mcam::default_pack_dir().expect("ManyCam pack dir not found");
    println!("pack dir: {}", dir.display());
    let packs = ovc_mcam::list_packs(&dir)?;
    for p in &packs {
        let effs = ovc_mcam::list_effects_in_pack(p).unwrap_or_default();
        println!("  {} — {} effects", p.file_stem().unwrap().to_string_lossy(), effs.len());
    }

    // Decode one object and one animation to prove end-to-end decode.
    for (pack, want_kind) in [("Sports", ovc_mcam::Kind::Object), ("Emojis", ovc_mcam::Kind::Animation)] {
        if let Some(p) = packs.iter().find(|p| p.file_stem().unwrap().to_string_lossy() == pack) {
            let effs = ovc_mcam::list_effects_in_pack(p)?;
            if let Some(r) = effs.iter().find(|r| r.kind == want_kind) {
                let e = ovc_mcam::load_effect(&r.pack_path, &r.mcv_name)?;
                println!(
                    "\n[{}] '{}' type={} kind={:?} frames={} first={}x{} delay={}ms pos={:?} size={:?}",
                    pack, e.name, e.raw_type, e.kind, e.frames.len(),
                    e.frames[0].w, e.frames[0].h, e.frames[0].delay_ms, e.position, e.size
                );
            }
        }
    }
    Ok(())
}
