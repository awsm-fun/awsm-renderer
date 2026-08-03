// scratch: verify EditorCommand JSON round-trip for the size-cap patch
#[test]
fn debug_seam_set_bundle_options_json() {
    let j = r#"{"cmd":"set_bundle_options","patch":{"env_max_face_size":1024}}"#;
    let cmd: awsm_renderer_editor_protocol::EditorCommand = serde_json::from_str(j).unwrap();
    let awsm_renderer_editor_protocol::EditorCommand::SetBundleOptions { patch } = cmd else {
        panic!("wrong variant");
    };
    assert_eq!(patch.env_max_face_size, Some(Some(1024)));
    let req_json =
        r#"{"Dispatch":{"cmd":"set_bundle_options","patch":{"env_max_face_size":1024}}}"#;
    let _req: awsm_renderer_editor_protocol::Request = serde_json::from_str(req_json).unwrap();
    let ov = r#"{"ExportPlayerBundle":{"overrides":{"env_max_face_size":1024}}}"#;
    let req: awsm_renderer_editor_protocol::Request = serde_json::from_str(ov).unwrap();
    let awsm_renderer_editor_protocol::Request::ExportPlayerBundle { overrides } = req else {
        panic!("wrong req variant");
    };
    assert_eq!(overrides.unwrap().env_max_face_size, Some(Some(1024)));
}
