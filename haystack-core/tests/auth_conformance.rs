//! Canonical RFC 7677 bytes, independently checked with Python stdlib.
//! See the fixture provenance. Do not trim transcripts to match the defective
//! Haystack illustrative message, which omits a nonce suffix and includes LF.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use haystack_core::auth;

fn fixture() -> serde_json::Value {
    serde_json::from_str(include_str!("fixtures/auth-conformance.json")).unwrap()
}

#[test]
fn rfc7677_outer_encoding_and_proof_match_independent_fixture() {
    let f = fixture();
    let published = &f["published"];
    let outer = &f["derived_outer"];
    let result = auth::client_final_message(
        published["password"].as_str().unwrap(),
        outer["client_first_data"].as_str().unwrap(),
        outer["server_first_data"].as_str().unwrap(),
    )
    .unwrap();
    assert_eq!(result.0, outer["client_final_data"]);
}

#[test]
fn hello_and_client_first_use_unpadded_base64url() {
    assert!(auth::parse_auth_header("HELLO username=am9zw6ksPeW3pQ").is_ok());
    let (_, first) = auth::client_first_message("user");
    assert!(URL_SAFE_NO_PAD.decode(first).is_ok());
}

#[test]
fn unicode_escaping_and_both_server_verifiers_match_independent_vectors() {
    let f = fixture();
    for (input, output) in [
        (&f["derived_outer"], &f["derived_outer"]),
        (&f["synthetic_unicode"], &f["synthetic_unicode"]),
    ] {
        let (proof, signature) = auth::client_final_message(
            "pencil",
            input["client_first_data"].as_str().unwrap(),
            f["derived_outer"]["server_first_data"].as_str().unwrap(),
        )
        .unwrap();
        assert_eq!(proof, output["client_final_data"]);
        auth::verify_server_final(output["server_final_data"].as_str().unwrap(), &signature)
            .unwrap();
    }
}

#[test]
fn bounded_malformed_transcripts_reject_without_echoing_input() {
    let first = URL_SAFE_NO_PAD.encode("n,,n=user,r=clientnonce");
    for server in [
        "r=clientnonce,s=c2FsdA==,i=4096",
        "r=wrong,s=c2FsdA==,i=4096",
        "r=clientnonceextra,s=,i=4096",
        "r=clientnonceextra,s=c2FsdA==,i=0",
        "r=clientnonceextra,s=c2FsdA==,i=1000001",
        "r=clientnonceextra,r=duplicate,s=c2FsdA==,i=4096",
        "r=clientnonceextra,s=c2FsdA==,i=4096\n",
    ] {
        let encoded = URL_SAFE_NO_PAD.encode(server);
        let error = auth::client_final_message("password", &first, &encoded).unwrap_err();
        assert!(!format!("{error:?} {error}").contains(server));
    }
    for bad in [
        URL_SAFE_NO_PAD.encode([255]),
        "A".repeat(9000),
        "private-sentinel==".into(),
    ] {
        assert!(auth::decode_auth_data(&bad).is_err());
        assert!(auth::extract_client_nonce(&bad).is_err());
    }
    for text in [
        "n,,n=user,r=nonce,r=duplicate",
        "n,,n=bad=escape,r=nonce",
        "n,,n=user,r=nonce\n",
        "y,,n=user,r=nonce",
        "n,,n=user,r=",
    ] {
        assert!(auth::extract_client_nonce(&URL_SAFE_NO_PAD.encode(text)).is_err());
    }
}

#[test]
fn proof_and_verifier_lengths_are_rejected_before_xor_or_comparison() {
    let credentials = auth::derive_credentials("pencil", b"salt", 4096);
    let first = URL_SAFE_NO_PAD.encode("n,,n=user,r=clientnonce");
    let (handshake, _) = auth::server_first_message("user", &first, &credentials).unwrap();
    let nonce = format!("{}{}", handshake.client_nonce, handshake.server_nonce);
    for proof in [
        "",
        "YQ==",
        "!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!",
        "YQ==,p=YQ==",
    ] {
        let encoded = URL_SAFE_NO_PAD.encode(format!("c=biws,r={nonce},p={proof}"));
        assert!(auth::server_verify_final(&handshake, &encoded).is_err());
        assert!(
            auth::verify_server_final(&URL_SAFE_NO_PAD.encode(format!("v={proof}")), &[0; 32])
                .is_err()
        );
    }
    assert!(
        auth::server_verify_final(
            &handshake,
            &URL_SAFE_NO_PAD.encode(format!("c=biws,r=wrong,p={}", "A".repeat(44)))
        )
        .is_err()
    );
}

#[test]
fn empty_http_parameters_are_bounded_without_relaxing_scram_transcripts() {
    for text in [
        ", hash=SHA-256",
        "hash=SHA-256, , data=YQ",
        "hash=SHA-256, ",
    ] {
        assert_eq!(
            auth::parse_auth_parameters(text).unwrap()["hash"],
            "SHA-256"
        );
    }
    for text in [
        "hash=SHA-256, ,hash=SHA-256",
        "hash=SHA-256, ,bad",
        "hash=SHA-256, ,data=",
    ] {
        assert!(auth::parse_auth_parameters(text).is_err());
    }
    assert!(auth::parse_auth_parameters(&format!("{}hash=SHA-256", ",".repeat(64))).is_err());
    assert!(auth::parse_auth_header("HELLO , ,").is_err());
    assert!(auth::parse_auth_header("SCRAM , ,").is_err());
    assert!(auth::parse_auth_header("BEARER , ,").is_err());
    let first = URL_SAFE_NO_PAD.encode("n,,n=user,r=nonce");
    for transcript in [
        "r=nonceextra,,s=c2FsdA==,i=4096",
        ",r=nonceextra,s=c2FsdA==,i=4096",
        "r=nonceextra,s=c2FsdA==,i=4096,",
    ] {
        assert!(auth::validate_server_first(&first, &URL_SAFE_NO_PAD.encode(transcript)).is_err());
    }
}
