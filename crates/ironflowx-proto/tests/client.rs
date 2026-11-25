//! What the proto client takes for a URL. Run twice: `https` is one only with `client-tls`.

use ironflowx_proto::{Client, InvalidUrl};

#[test]
fn a_url_is_http_with_a_host_and_no_user_query_or_fragment() {
    for url in [
        "http://127.0.0.1:9310",
        "http://127.0.0.1:9310/",
        "http://localhost",
        "http://[::1]:9310",
        "http://proxy.example/ironflow",
        "http://h:65535",
        "http://h:0080",
        "http://[::ffff:127.0.0.1]",
    ] {
        assert!(Client::new(url).is_ok(), "{url}");
    }
    for url in [
        "https://127.0.0.1:9310",
        "https://localhost",
        "HTTPS://[::1]:9310/ironflow",
    ] {
        let tls = cfg!(feature = "client-tls");
        assert_eq!(
            Client::new(url).err(),
            (!tls).then_some(InvalidUrl),
            "{url}"
        );
    }
    for url in [
        "",
        "127.0.0.1:9310",
        "/path",
        "http://",
        "http:///path",
        "http://:80",
        "ftp://127.0.0.1",
        "https://",
        "https://user:pass@h",
        "https://h?token=x",
        "https://h#x",
        "https://h:65536",
        "httpss://h",
        "unix:/tmp/x",
        "http://user@h",
        "http://user:pass@h",
        "http://h?token=x",
        "http://h/?",
        "http://h/#x",
        "http://h#x",
        "http://h:",
        "http://h:65536",
        "http://h:x",
        "http://h:+80",
        "http://h:-1",
        "http://[bad]:80",
        "http://[::1",
        "http://[::1]x",
        "http://[]",
        "http://[::1]]",
        "http://[fe80::1%25en0]",
        "http://u@[::1]:80",
        "http://h@h",
        "http://h@h:80",
        "http://host-user:pw@host:80/",
        "http://[::1]x:80",
        "http://h:80:80",
        "http:// h",
        "http://h /",
    ] {
        assert_eq!(Client::new(url).err(), Some(InvalidUrl), "{url}");
    }
    let form = match cfg!(feature = "client-tls") {
        true => "not a URL of the form http[s]://host[:port][/path]",
        false => "not a URL of the form http://host[:port][/path]",
    };
    assert_eq!(InvalidUrl.to_string(), form);
}
