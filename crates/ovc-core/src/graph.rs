//! The processing graph: a DAG of nodes connected by frame edges.
//!
//! Scheduling is **pull-based**: a sink's cadence drives [`Pipeline::tick`], which walks
//! nodes in topological order, handing each node its inputs and collecting its output.

use std::collections::HashMap;

use crate::frame::FrameRef;
use crate::time::Timestamp;
use crate::Result;

/// Stable identifier for a node within a pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Source,
    Transform,
    Sink,
}

/// Per-tick context handed to a node: its ordered inputs and the frame timestamp.
pub struct ProcessCtx<'a> {
    pub pts: Timestamp,
    pub inputs: &'a [FrameRef],
}

/// A processing node. Implementors live in `ovc-capture`, `ovc-effects`, `ovc-vcam`, …
pub trait Node: Send {
    fn kind(&self) -> NodeKind;

    /// Produce this node's output for the current tick.
    ///
    /// - `Source` nodes ignore `ctx.inputs`.
    /// - `Sink` nodes return `Ok(None)`.
    fn process(&mut self, ctx: ProcessCtx<'_>) -> Result<Option<FrameRef>>;
}

/// A pipeline owns its nodes and the edges between them.
#[derive(Default)]
pub struct Pipeline {
    nodes: HashMap<NodeId, Box<dyn Node>>,
    /// `edges[dst]` = ordered list of source node ids feeding `dst`.
    edges: HashMap<NodeId, Vec<NodeId>>,
    order: Vec<NodeId>,
    next_id: u32,
}

impl Pipeline {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, node: Box<dyn Node>) -> NodeId {
        let id = NodeId(self.next_id);
        self.next_id += 1;
        self.nodes.insert(id, node);
        self.edges.entry(id).or_default();
        id
    }

    /// Connect `from` → `to` (output of `from` becomes an input of `to`).
    pub fn connect(&mut self, from: NodeId, to: NodeId) {
        self.edges.entry(to).or_default().push(from);
    }

    /// Recompute topological order. Call after mutating the graph.
    pub fn finalize(&mut self) -> Result<()> {
        self.order = topo_sort(&self.nodes, &self.edges)?;
        Ok(())
    }

    /// Run one frame through the whole graph.
    pub fn tick(&mut self, pts: Timestamp) -> Result<()> {
        let mut outputs: HashMap<NodeId, FrameRef> = HashMap::new();
        for &id in &self.order {
            let inputs: Vec<FrameRef> = self.edges[&id]
                .iter()
                .filter_map(|src| outputs.get(src).cloned())
                .collect();
            let node = self.nodes.get_mut(&id).expect("node present");
            if let Some(out) = node.process(ProcessCtx { pts, inputs: &inputs })? {
                outputs.insert(id, out);
            }
        }
        Ok(())
    }
}

fn topo_sort(
    nodes: &HashMap<NodeId, Box<dyn Node>>,
    edges: &HashMap<NodeId, Vec<NodeId>>,
) -> Result<Vec<NodeId>> {
    // Kahn's algorithm over the reversed (src→dst) edges.
    let mut indeg: HashMap<NodeId, usize> = nodes.keys().map(|&n| (n, 0)).collect();
    for (&dst, srcs) in edges {
        *indeg.entry(dst).or_default() += srcs.len();
    }
    let mut ready: Vec<NodeId> = indeg
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(&n, _)| n)
        .collect();
    let mut order = Vec::with_capacity(nodes.len());
    while let Some(n) = ready.pop() {
        order.push(n);
        for (&dst, srcs) in edges {
            if srcs.contains(&n) {
                let e = indeg.get_mut(&dst).unwrap();
                *e -= 1;
                if *e == 0 {
                    ready.push(dst);
                }
            }
        }
    }
    if order.len() == nodes.len() {
        Ok(order)
    } else {
        Err(crate::Error::InvalidGraph)
    }
}
