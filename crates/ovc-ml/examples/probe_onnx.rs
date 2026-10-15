//! Probe ONNX model input/output shapes with tract, and run once on zeros to reveal
//! concrete output shapes. Run: `cargo run -p ovc-ml --example probe_onnx -- a.onnx b.onnx`

use tract_onnx::prelude::*;
use tract_onnx::tract_hir::infer::Factoid;

fn main() -> TractResult<()> {
    for path in std::env::args().skip(1) {
        println!("\n===== {path} =====");
        let infer = tract_onnx::onnx().model_for_path(&path)?;
        for (i, &o) in infer.inputs.iter().enumerate() {
            println!("  input  {i}: {:?}", infer.outlet_fact(o)?);
        }
        for (i, &o) in infer.outputs.iter().enumerate() {
            println!("  output {i} (declared): {:?}", infer.outlet_fact(o)?);
        }
        // Try to run on a zero input to see concrete output shapes.
        let in_fact = infer.outlet_fact(infer.inputs[0])?.clone();
        let dims: Vec<usize> = in_fact
            .shape
            .dims()
            .map(|d| d.concretize().and_then(|t| t.to_i64().ok()).map(|v| v.max(1) as usize).unwrap_or(1))
            .collect();
        match infer.clone().into_optimized().and_then(|m| m.into_runnable()) {
            Ok(runnable) => {
                let zeros = Tensor::zero::<f32>(&dims)?;
                match runnable.run(tvec!(zeros.into())) {
                    Ok(outs) => {
                        for (i, o) in outs.iter().enumerate() {
                            println!("  output {i} (ran zeros): shape {:?}", o.shape());
                        }
                    }
                    Err(e) => println!("  run failed: {e}"),
                }
            }
            Err(e) => println!("  optimize/runnable failed: {e}"),
        }
    }
    Ok(())
}
