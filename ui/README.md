# ui

Desktop UI frontend for the Tauri app (`crates/ovc-app`, added once the engine API
stabilizes).

Planned stack: Svelte + TypeScript, Vite. The Tauri Rust backend owns the engine
(`ovc-core` pipeline, `ovc-gpu` device) and exposes commands/events to this frontend:

- source & virtual-camera selection,
- an effect stack (add / reorder / toggle / parameter sliders), bound to
  `ovc-effects::Registry`,
- live preview (GPU surface rendered by the backend, or a streamed preview),
- scenes / presets saved as JSON project files.

`npm create vite@latest` here when work begins.
