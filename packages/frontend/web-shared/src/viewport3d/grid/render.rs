use awsm_renderer::{
    error::AwsmError, pipelines::render_pipeline::RenderPipelineKey, render::RenderContext,
};

pub fn render_grid(
    ctx: &RenderContext,
    grid_bind_group: &web_sys::GpuBindGroup,
    grid_pipeline_key: RenderPipelineKey,
) -> std::result::Result<(), AwsmError> {
    let render_pass = ctx.begin_world_transparent_pass(Some("Grid Render Pass"))?;

    render_pass.set_bind_group(0, grid_bind_group, None)?;

    render_pass.set_pipeline(ctx.pipelines.render.get(grid_pipeline_key)?);
    render_pass.draw(3);
    render_pass.end();

    Ok(())
}
