//! Dev probe: print every effect name per ManyCam pack.
//! Run: `cargo run -p ovc-mcam --example names`

fn main() -> anyhow::Result<()> {
    let dir = ovc_mcam::default_pack_dir().expect("ManyCam pack dir not found");
    for p in ovc_mcam::list_packs(&dir)? {
        let effs = ovc_mcam::list_effects_in_pack(&p).unwrap_or_default();
        let names: Vec<String> = effs.iter().map(|r| format!("{} ({:?})", r.display_name, r.kind)).collect();
        println!("## {} [{}]\n{}\n", p.file_stem().unwrap().to_string_lossy(), effs.len(), names.join(", "));
    }
    Ok(())
}
