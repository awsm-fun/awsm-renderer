//! Insert action-row — node insertions (Empty / Model / Light /
//! Collision / Camera / Primitive / Curve / Visual) plus the
//! `+ Material Asset` shared-asset shortcut.

use super::menu::render_dropdown_button;
use crate::{actions, prelude::*};

pub(super) fn render_insert_row() -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("gap", "0.5rem")
        .style("align-items", "center")
        .child(Button::new()
            .with_text("Empty")
            .with_style(ButtonStyle::Outline)
            .with_size(ButtonSize::Sm)
            .with_on_click(actions::insert::empty)
            .render())
        .child(render_insert_model_button())
        .child(render_dropdown_button("Light…", vec![
            ("Directional", Arc::new(|| actions::insert::light(actions::insert::LightKind::Directional))),
            ("Point", Arc::new(|| actions::insert::light(actions::insert::LightKind::Point))),
            ("Spot", Arc::new(|| actions::insert::light(actions::insert::LightKind::Spot))),
        ]))
        .child(render_dropdown_button("Collision…", vec![
            ("Box", Arc::new(actions::insert::collision_box)),
            ("Sphere", Arc::new(actions::insert::collision_sphere)),
            ("Capsule", Arc::new(actions::insert::collision_capsule)),
            ("Cylinder", Arc::new(actions::insert::collision_cylinder)),
            ("Cone", Arc::new(actions::insert::collision_cone)),
            ("Ellipsoid", Arc::new(actions::insert::collision_ellipsoid)),
        ]))
        .child(Button::new()
            .with_text("Camera")
            .with_style(ButtonStyle::Outline)
            .with_size(ButtonSize::Sm)
            .with_on_click(actions::insert::camera)
            .render())
        // Procedural primitives — load through `awsm-meshgen` at
        // materialize time; each variant ships with its default param
        // set so the node renders out of the box.
        .child(render_dropdown_button("Primitive…", vec![
            ("Plane", Arc::new(actions::insert::primitive_plane)),
            ("Box", Arc::new(actions::insert::primitive_box)),
            ("Sphere", Arc::new(actions::insert::primitive_sphere)),
            ("Cylinder", Arc::new(actions::insert::primitive_cylinder)),
            ("Cone", Arc::new(actions::insert::primitive_cone)),
            ("Torus", Arc::new(actions::insert::primitive_torus)),
        ]))
        // Curves + their consumers. Sweep + Instances both reference
        // a Curve `NodeId`; the user picks it via the inspector after
        // insert.
        .child(render_dropdown_button("Curve…", vec![
            ("Curve", Arc::new(actions::insert::curve)),
            ("Sweep along curve", Arc::new(actions::insert::sweep)),
            ("Instances along curve", Arc::new(actions::insert::instances)),
        ]))
        // Visual primitives — Line is the fat-line pipeline, Sprite is
        // the billboarded textured quad, Particle is the simulator-
        // backed emitter.
        .child(render_dropdown_button("Visual…", vec![
            ("Line", Arc::new(actions::insert::line)),
            ("Sprite", Arc::new(actions::insert::sprite)),
            ("Particle Emitter", Arc::new(actions::insert::particle)),
            ("Decal", Arc::new(actions::insert::decal)),
            ("Shared Mesh", Arc::new(actions::insert::mesh)),
        ]))
        // Shared assets — distinct from node insertions because they
        // don't add a tree entry, they add an `AssetSource::Material`
        // entry into the project's asset table. The Primitive / Sweep
        // / Mesh inspectors pick from these via the material-ref dropdown.
        .child(Button::new()
            .with_text("+ Material Asset")
            .with_style(ButtonStyle::Outline)
            .with_size(ButtonSize::Sm)
            .with_on_click(|| { let _ = actions::insert::material_asset(); })
            .render())
    })
}

fn render_insert_model_button() -> Dom {
    let file_input: Mutable<Option<web_sys::HtmlInputElement>> = Mutable::new(None);

    html!("div", {
        .style("display", "inline-flex")
        .child(Button::new()
            .with_text("Model…")
            .with_style(ButtonStyle::Outline)
            .with_size(ButtonSize::Sm)
            .with_on_click(clone!(file_input => move || {
                if let Some(input) = file_input.get_cloned() {
                    input.click();
                }
            }))
            .render())
        .child(html!("input" => web_sys::HtmlInputElement, {
            .attr("type", "file")
            .attr("accept", ".glb,.gltf")
            .style("display", "none")
            .with_node!(input => {
                .after_inserted(clone!(file_input, input => move |_| {
                    file_input.set(Some(input));
                }))
                .after_removed(clone!(file_input => move |_| {
                    file_input.set(None);
                }))
                .event(clone!(input => move |_: events::Change| {
                    let file = input.files().and_then(|files| files.get(0));
                    input.set_value("");
                    if let Some(file) = file {
                        actions::insert::model(file);
                    }
                }))
            })
        }))
    })
}
