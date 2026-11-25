//! All four custom options of service.proto are in what its build script generates.

#[test]
fn every_option_reaches_the_generator() {
    let code = include_str!(concat!(env!("OUT_DIR"), "/cli.rs"));
    for option in [
        r#"path: "events emit", summary: "Emit an event","#,
        r#"// Example: {"data":"{\"id\": 1}","event_name":"order.placed"}"#,
        r#""data":{"contentMediaType":"application/json","type":"string"}"#,
    ] {
        assert!(code.contains(option), "{option} is not in {code}");
    }
}
