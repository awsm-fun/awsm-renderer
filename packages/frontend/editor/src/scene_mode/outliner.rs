//! Outliner: the scene tree — kind icon · name · eye · lock,
//! group collapse, single/ctrl/shift select, per-row context menu, empty state,
//! filter, and an add button. Bound to `controller().scene`; every mutation is a
//! dispatched command (selection is transient; visibility/lock/prefab/duplicate/
//! delete are undoable). Drag-reparent lands as a follow-up.

use std::sync::Arc;

use awsm_editor_protocol::PrimitiveShape;

use std::cell::RefCell;

use crate::controller::InsertSpec;
use crate::engine::scene::mutate::{
    ancestor_dedup, find_by_id, find_parent, flatten_visible_order,
};
use crate::engine::scene::{Node, NodeId, NodeKind};
use crate::prelude::*;

thread_local! {
    /// The node set captured at the start of an outliner drag — the whole
    /// (ancestor-deduped) selection if the grabbed row is part of it, else just
    /// the grabbed node. Snapshotting at drag-start (rather than reading the
    /// selection at drop-time) makes a multi-node drag robust against any
    /// selection change the pointer interaction might trigger mid-drag. Same-app
    /// drag, so no HTML5 `dataTransfer`; cleared on drop / drag-end.
    static DRAG_SET: RefCell<Vec<NodeId>> = const { RefCell::new(Vec::new()) };
}

/// The nodes a drag starting on `src` should move: the whole selection (deduped
/// to top-most ancestors) if `src` is selected, else just `src`.
fn selection_aware_ids(src: NodeId) -> Vec<NodeId> {
    let ctrl = controller();
    let selection = ctrl.selected.get_cloned();
    if selection.contains(&src) {
        ancestor_dedup(&ctrl.scene, selection.iter().copied())
    } else {
        vec![src]
    }
}

/// Reparent every node in `ids` under `new_parent` (`None` = scene root) as one
/// sequential transaction, then expand the target so the moved nodes are visible
/// (a collapsed Empty would otherwise look like the drop did nothing).
/// `mutate::reparent` guards cycles + self-parenting, so invalid moves no-op.
fn reparent_nodes(ids: Vec<NodeId>, new_parent: Option<NodeId>) {
    if ids.is_empty() {
        return;
    }
    spawn_local(async move {
        let ctrl = controller();
        for id in ids {
            if Some(id) == new_parent {
                continue;
            }
            let _ = ctrl
                .dispatch(EditorCommand::Reparent {
                    id,
                    new_parent,
                    index: None,
                })
                .await;
        }
        if let Some(parent) = new_parent {
            if let Some(node) = find_by_id(&controller().scene, parent) {
                node.expanded.set(true);
                controller().scene.bump_revision();
            }
        }
    });
}

fn reparent_into(new_parent: Option<NodeId>, src: NodeId) {
    reparent_nodes(selection_aware_ids(src), new_parent);
}

/// Snapshot the drag set when a drag begins on `src`.
fn begin_drag(src: NodeId) {
    DRAG_SET.with(|c| *c.borrow_mut() = selection_aware_ids(src));
}

/// True if a drag is in flight (any captured nodes).
fn drag_active() -> bool {
    DRAG_SET.with(|c| !c.borrow().is_empty())
}

/// True if a drag is in flight and `target` is a valid drop target (not one of
/// the dragged nodes — you can't drop a node onto itself / its own selection).
fn drag_active_for(target: NodeId) -> bool {
    DRAG_SET.with(|c| {
        let s = c.borrow();
        !s.is_empty() && !s.contains(&target)
    })
}

/// Commit the in-flight drag under `new_parent` (taking + clearing the set).
fn drop_drag(new_parent: Option<NodeId>) {
    let ids = DRAG_SET.with(|c| std::mem::take(&mut *c.borrow_mut()));
    reparent_nodes(ids, new_parent);
}

fn clear_drag() {
    DRAG_SET.with(|c| c.borrow_mut().clear());
}

/// Wrap the current selection (or `seed` if nothing's selected) in a fresh Empty
/// parent, created under the first selected node's parent so world positions are
/// preserved, then reparent the selection under it. This is the "create a new
/// parent to contain these nodes" action.
fn group_selection(seed: NodeId) {
    let ctrl = controller();
    let mut selection = ctrl.selected.get_cloned();
    if selection.is_empty() {
        selection = vec![seed];
    }
    let ids = ancestor_dedup(&ctrl.scene, selection.iter().copied());
    if ids.is_empty() {
        return;
    }
    // Place the group where the first grouped node already lives, so grouping
    // doesn't yank the nodes across the hierarchy.
    let parent = find_parent(&ctrl.scene, ids[0]).map(|p| p.id);
    let group_id = NodeId::new();
    spawn_local(async move {
        let ctrl = controller();
        if ctrl
            .dispatch(EditorCommand::Insert {
                id: group_id,
                spec: InsertSpec::Empty,
                parent,
            })
            .await
            .is_err()
        {
            return;
        }
        for id in ids {
            let _ = ctrl
                .dispatch(EditorCommand::Reparent {
                    id,
                    new_parent: Some(group_id),
                    index: None,
                })
                .await;
        }
        let _ = ctrl
            .dispatch(EditorCommand::SetSelection {
                ids: vec![group_id],
            })
            .await;
    });
}

pub fn render() -> Dom {
    let ctrl = controller();
    let filter = Mutable::new(String::new());

    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("height", "100%")
        .style("background", "var(--bg-1)")
        .child(header())
        .child(html!("div", {
            .style("padding", "8px 10px")
            .child(TextInput::new(filter.clone()).placeholder("Filter\u{2026}").icon("search").render())
        }))
        .child(html!("div", {
            .style("flex", "1")
            .style("overflow-y", "auto")
            .style("padding", "0 6px 8px")
            // Dropping on the empty area below the rows reparents to the scene
            // root (un-parent). Row drops stop propagation, so this only fires
            // for the background.
            .event(move |e: events::DragOver| {
                if drag_active() {
                    e.prevent_default();
                }
            })
            .event(move |e: events::Drop| {
                e.prevent_default();
                drop_drag(None);
            })
            // Rebuild the row list when the scene structure (revision) or the
            // filter changes; per-row selection/visibility bindings are reactive
            // so selection changes don't rebuild the list.
            .child_signal(map_ref! {
                let _rev = ctrl.scene.revision.signal(),
                let q = filter.signal_cloned() => move {
                    Some(tree_rows(q))
                }
            })
        }))
        .child(footer(&ctrl))
    })
}

fn header() -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("align-items", "center")
        .style("height", "38px")
        .style("padding", "0 8px 0 14px")
        .style("border-bottom", "1px solid var(--line-soft)")
        .style("flex", "0 0 auto")
        .child(html!("span", {
            .style("font-size", "12.5px")
            .style("font-weight", "620")
            .style("color", "var(--text-0)")
            .style("letter-spacing", "0.01em")
            .text("Outliner")
        }))
        .child(html!("div", {
            .style("margin-left", "auto")
            .style("display", "flex")
            .child(DropButton::new()
                .icon("plus")
                .chevron(false)
                .items(add_menu)
                .render())
        }))
    })
}

fn add_menu(close: Close) -> Vec<Dom> {
    let item = |label: &str, icon: &str, spec: InsertSpec, close: Close| -> Dom {
        MenuItem::new(label)
            .icon(icon)
            .on_click(move || {
                let spec = spec.clone();
                spawn_local(async move {
                    let _ = controller()
                        .dispatch(EditorCommand::Insert {
                            id: awsm_editor_protocol::NodeId::new(),
                            spec,
                            parent: None,
                        })
                        .await;
                });
                (close.borrow_mut())();
            })
            .render()
    };
    vec![
        item("Empty", "empty", InsertSpec::Empty, close.clone()),
        item("Camera", "camera", InsertSpec::Camera, close.clone()),
        menu_sep(),
        item(
            "Primitive · Sphere",
            "sphere",
            InsertSpec::Primitive(PrimitiveShape::default_sphere()),
            close.clone(),
        ),
        item(
            "Primitive · Box",
            "cube",
            InsertSpec::Primitive(PrimitiveShape::default_box()),
            close.clone(),
        ),
        item(
            "Light · Directional",
            "light",
            InsertSpec::Light(awsm_editor_protocol::LightKind::Directional),
            close.clone(),
        ),
        item(
            "Light · Point",
            "light",
            InsertSpec::Light(awsm_editor_protocol::LightKind::Point),
            close,
        ),
    ]
}

fn footer(ctrl: &EditorController) -> Dom {
    html!("div", {
        .style("padding", "8px 10px")
        .style("border-top", "1px solid var(--line-soft)")
        .style("display", "flex")
        .style("align-items", "center")
        .style("gap", "6px")
        .child(html!("span", {
            .class("kicker")
            .style("font-size", "10px")
            .text_signal(map_ref! {
                let _rev = ctrl.scene.revision.signal(),
                let sel = ctrl.selected.signal_cloned() => {
                    let count = scene_node_count();
                    let suffix = if sel.len() > 1 { format!(" \u{00b7} {} selected", sel.len()) } else { String::new() };
                    format!("{count} object{}{suffix}", if count == 1 { "" } else { "s" })
                }
            })
        }))
    })
}

fn scene_node_count() -> usize {
    fn walk(nodes: &[Arc<Node>]) -> usize {
        nodes
            .iter()
            .map(|n| 1 + walk(n.children.lock_ref().as_slice()))
            .sum()
    }
    walk(controller().scene.nodes.lock_ref().as_slice())
}

/// Build the flat row list (respecting collapse, or flat when filtering).
fn tree_rows(filter: &str) -> Dom {
    let q = filter.trim().to_lowercase();
    let mut rows: Vec<(Arc<Node>, usize)> = Vec::new();
    collect_rows(
        controller().scene.nodes.lock_ref().as_slice(),
        0,
        &q,
        &mut rows,
    );

    if rows.is_empty() && q.is_empty() {
        return empty_state();
    }

    html!("div", {
        .children(rows.into_iter().map(|(node, depth)| row(node, depth)))
    })
}

fn collect_rows(nodes: &[Arc<Node>], depth: usize, q: &str, out: &mut Vec<(Arc<Node>, usize)>) {
    for node in nodes {
        if q.is_empty() {
            out.push((node.clone(), depth));
            if node.expanded.get() {
                collect_rows(node.children.lock_ref().as_slice(), depth + 1, q, out);
            }
        } else {
            if node.name.get_cloned().to_lowercase().contains(q) {
                out.push((node.clone(), depth));
            }
            collect_rows(node.children.lock_ref().as_slice(), depth + 1, q, out);
        }
    }
}

fn row(node: Arc<Node>, depth: usize) -> Dom {
    let id = node.id;
    let has_kids = !node.children.lock_ref().is_empty();
    let ctx_open: Mutable<Option<(f64, f64)>> = Mutable::new(None);
    let hover = Mutable::new(false);
    // Highlight when a drag hovers over this row (it's a drop target).
    let drag_over = Mutable::new(false);

    // selection signals
    let sel = controller().selected.clone();
    let bg_sig = sel.signal_cloned().map(move |s| s.contains(&id));
    let primary_sig = sel.signal_cloned().map(move |s| s.last() == Some(&id));

    html!("div", {
        .style("display", "flex")
        .style("align-items", "center")
        .style("gap", "6px")
        .style("height", "28px")
        .style("padding-left", &format!("{}px", 6 + depth * 15))
        .style("padding-right", "6px")
        .style("border-radius", "var(--r1)")
        .style("cursor", "pointer")
        .style("position", "relative")
        .style_signal("background", map_ref! {
            let on = bg_sig, let h = hover.signal() => {
                if *on { "var(--accent-ghost)" } else if *h { "var(--bg-hover)" } else { "transparent" }
            }
        })
        .style_signal("box-shadow", primary_sig.map(|p| if p { "inset 2px 0 0 var(--accent)" } else { "none" }))
        // Drop-target ring while a drag hovers over this row.
        .style_signal("outline", drag_over.signal().map(|d| if d { "2px solid var(--accent)" } else { "none" }))
        .style("outline-offset", "-2px")
        // Drag-to-reparent: this row is both a draggable source and a drop target.
        .attr("draggable", "true")
        .event(move |_: events::DragStart| begin_drag(id))
        .event(move |_: events::DragEnd| clear_drag())
        .event(clone!(drag_over => move |e: events::DragOver| {
            // A real drag whose set doesn't include this row → a valid drop here.
            if drag_active_for(id) {
                e.prevent_default();
                drag_over.set_neq(true);
            }
        }))
        .event(clone!(drag_over => move |_: events::DragLeave| drag_over.set_neq(false)))
        .event(clone!(drag_over => move |e: events::Drop| {
            e.prevent_default();
            e.stop_propagation();
            drag_over.set_neq(false);
            if drag_active_for(id) {
                drop_drag(Some(id));
            }
        }))
        .event(clone!(hover => move |_: events::MouseEnter| hover.set_neq(true)))
        .event(clone!(hover => move |_: events::MouseLeave| hover.set_neq(false)))
        .event(move |e: events::Click| {
            let additive = e.ctrl_key();
            let range = e.shift_key();
            select_node(id, additive, range);
        })
        .event(clone!(ctx_open => move |e: events::ContextMenu| {
            e.prevent_default();
            e.stop_propagation();
            ctx_open.set(Some((e.x(), e.y())));
        }))
        // collapse chevron (only when the node has children)
        .child(if has_kids {
            html!("div", {
                .style("width", "13px")
                .style("flex", "0 0 auto")
                .style("display", "flex")
                .style("cursor", "pointer")
                .style("color", "var(--text-3)")
                .event(clone!(node => move |e: events::Click| {
                    e.stop_propagation();
                    node.expanded.set(!node.expanded.get());
                    // structural change → bump so the list rebuilds
                    controller().scene.bump_revision();
                }))
                .child_signal(node.expanded.signal().map(|open| Some(
                    Icon::new("chevron").size(12.0)
                        .style("transform", if open { "rotate(90deg)" } else { "none" })
                        .style("transition", "transform .12s")
                        .render()
                )))
            })
        } else {
            html!("span", { .style("width", "13px").style("flex", "0 0 auto") })
        })
        .child(Icon::new(row_icon(&node)).size(15.0).color("var(--text-2)").style("flex", "0 0 auto").render())
        .child(html!("span", {
            .style("flex", "1")
            .style("font-size", "12.5px")
            .style("white-space", "nowrap")
            .style("overflow", "hidden")
            .style("text-overflow", "ellipsis")
            .style_signal("color", node.visible.signal().map(|v| if v { "var(--text-1)" } else { "var(--text-3)" }))
            .text_signal(node.name.signal_cloned())
        }))
        // prefab tag
        .child_signal(node.prefab.signal().map(|p| if p {
            Some(html!("span", {
                .attr("title", "Prefab root")
                .style("font-size", "9px").style("font-weight", "700")
                .style("color", "var(--accent-bright)").style("letter-spacing", ".04em")
                .text("PF")
            }))
        } else { None }))
        // eye
        .child(html!("button", {
            .class("t")
            .attr("title", "Visibility")
            .style("background", "transparent").style("border-style", "none").style("cursor", "pointer")
            .style("color", "var(--text-3)").style("display", "flex").style("padding", "2px")
            .style_signal("opacity", node.visible.signal().map(|v| if v { "0.55" } else { "1" }))
            .event(clone!(node => move |e: events::Click| {
                e.stop_propagation();
                let v = node.visible.get();
                spawn_local(async move { let _ = controller().dispatch(EditorCommand::SetVisible { id, visible: !v }).await; });
            }))
            .child_signal(node.visible.signal().map(|v| Some(Icon::new(if v { "eye" } else { "eyeoff" }).size(14.0).render())))
        }))
        // lock
        .child(html!("button", {
            .class("t")
            .attr("title", "Lock")
            .style("background", "transparent").style("border-style", "none").style("cursor", "pointer")
            .style("display", "flex").style("padding", "2px")
            .style_signal("color", node.locked.signal().map(|l| if l { "var(--text-1)" } else { "var(--text-3)" }))
            .style_signal("opacity", node.locked.signal().map(|l| if l { "1" } else { "0.5" }))
            .event(clone!(node => move |e: events::Click| {
                e.stop_propagation();
                let l = node.locked.get();
                spawn_local(async move { let _ = controller().dispatch(EditorCommand::SetLocked { id, locked: !l }).await; });
            }))
            .child_signal(node.locked.signal().map(|l| Some(Icon::new(if l { "lock" } else { "unlock" }).size(14.0).render())))
        }))
        // context menu
        .child_signal(ctx_open.signal().map(clone!(node, ctx_open => move |pt| {
            pt.map(|(x, y)| row_context_menu(node.clone(), x, y, ctx_open.clone()))
        })))
    })
}

fn row_context_menu(node: Arc<Node>, x: f64, y: f64, open: Mutable<Option<(f64, f64)>>) -> Dom {
    let id = node.id;
    let close = {
        let open = open.clone();
        move || open.set(None)
    };
    let vis = node.visible.get();
    let locked = node.locked.get();
    let prefab = node.prefab.get();
    let dispatch = |cmd: EditorCommand| {
        spawn_local(async move {
            let _ = controller().dispatch(cmd).await;
        })
    };

    let rows = vec![
        MenuItem::new("Rename").icon("code").on_click(clone!(close => move || { select_node(id, false, false); close(); })).render(),
        MenuItem::new("Duplicate").icon("copy").hint("\u{2318}D").on_click(clone!(close => move || { dispatch(EditorCommand::Duplicate { id }); close(); })).render(),
        MenuItem::new(if locked { "Unlock" } else { "Lock" }).icon(if locked { "unlock" } else { "lock" })
            .on_click(clone!(close => move || { dispatch(EditorCommand::SetLocked { id, locked: !locked }); close(); })).render(),
        MenuItem::new(if vis { "Hide" } else { "Show" }).icon(if vis { "eyeoff" } else { "eye" })
            .on_click(clone!(close => move || { dispatch(EditorCommand::SetVisible { id, visible: !vis }); close(); })).render(),
        MenuItem::new(if prefab { "Unmark prefab" } else { "Mark as prefab" }).icon("layers")
            .on_click(clone!(close => move || { dispatch(EditorCommand::SetPrefab { id, prefab: !prefab }); close(); })).render(),
        menu_sep(),
        // Reparenting. "Group" wraps the selection in a new Empty parent;
        // "Move to root" un-parents. (Drag-and-drop in the tree also reparents.)
        MenuItem::new("Group selection").icon("layers")
            .on_click(clone!(close => move || { group_selection(id); close(); })).render(),
        MenuItem::new("Move to root").icon("chevron")
            .on_click(clone!(close => move || { reparent_into(None, id); close(); })).render(),
        menu_sep(),
        MenuItem::new("Delete").icon("trash").danger(true).hint("\u{232b}")
            .on_click(clone!(close => move || { dispatch(EditorCommand::Delete { id }); close(); })).render(),
    ];
    context_menu(x, y, move || open.set(None), rows)
}

fn empty_state() -> Dom {
    html!("div", {
        .style("display", "flex").style("flex-direction", "column").style("align-items", "center")
        .style("gap", "10px").style("padding", "40px 18px").style("text-align", "center")
        .child(html!("div", {
            .style("width", "40px").style("height", "40px").style("border-radius", "var(--r3)")
            .style("background", "var(--bg-3)").style("border", "1px solid var(--line)")
            .style("display", "flex").style("align-items", "center").style("justify-content", "center")
            .style("color", "var(--text-3)")
            .child(Icon::new("layers").size(20.0).render())
        }))
        .child(html!("div", {
            .style("font-size", "12.5px").style("color", "var(--text-2)").style("line-height", "1.5")
            .text("Your scene is empty. Insert your first object to get started.")
        }))
        .child(Btn::new().label("Add a Sphere").icon("sphere").variant(BtnVariant::Primary).size(BtnSize::Sm)
            .on_click(|| spawn_local(async {
                let _ = controller().dispatch(EditorCommand::Insert {
                    id: awsm_editor_protocol::NodeId::new(),
                    spec: InsertSpec::Primitive(PrimitiveShape::default_sphere()),
                    parent: None,
                }).await;
            })).render())
    })
}

/// Compute + dispatch the new selection for a row click.
fn select_node(id: NodeId, additive: bool, range: bool) {
    let ctrl = controller();
    let current = ctrl.selected.get_cloned();
    let new = if additive {
        if current.contains(&id) {
            current.into_iter().filter(|x| *x != id).collect()
        } else {
            let mut c = current;
            c.push(id);
            c
        }
    } else if range && !current.is_empty() {
        let anchor = *current.last().unwrap();
        let order = flatten_visible_order(&ctrl.scene);
        match (
            order.iter().position(|x| *x == anchor),
            order.iter().position(|x| *x == id),
        ) {
            (Some(a), Some(b)) => {
                let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                order[lo..=hi].to_vec()
            }
            _ => vec![id],
        }
    } else {
        vec![id]
    };
    spawn_local(async move {
        let _ = ctrl
            .dispatch(EditorCommand::SetSelection { ids: new })
            .await;
    });
}

/// Row icon: `kind_icon`, except skin-joint mirror bones (plain Groups in the
/// scene model) get the bone glyph — the bridge's joint registry knows which
/// Group ids are bones, so no NodeKind change is needed.
fn row_icon(node: &Arc<Node>) -> &'static str {
    let kind = node.kind.get_cloned();
    if matches!(kind, NodeKind::Group)
        && crate::engine::bridge::bridge()
            .skin_joint_baked
            .lock()
            .unwrap()
            .contains_key(&node.id)
    {
        return "bone";
    }
    kind_icon(&kind)
}

pub fn kind_icon(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Group => "empty",
        NodeKind::Light(_) => "light",
        NodeKind::Collider(_) => "collision",
        NodeKind::Camera(_) => "camera",
        NodeKind::Mesh { .. } => "cube",
        // No dedicated rig/skeleton glyph in the icon set; a skinned mesh is
        // still a mesh, so reuse the cube.
        NodeKind::SkinnedMesh { .. } => "cube",
        NodeKind::Curve(_) => "curve",
        NodeKind::InstancesAlongCurve(_) => "layers",
        NodeKind::Line(_) => "curve",
        NodeKind::Sprite(_) => "sprite",
        NodeKind::ParticleEmitter(_) => "sprite",
        NodeKind::Decal(_) => "texture",
    }
}
