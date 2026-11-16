use super::ReferAuthority;
use crate::ReferFailure;
use rusterisk_config::{Auth, Config, Endpoint};

fn configuration(
    profile: &str,
    realm: &str,
    username: &str,
    context: &str,
    allowed: bool,
) -> Result<(Endpoint, Auth), String> {
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test\n",
        &format!("[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:5060\n[{profile}]\ntype=auth\nusername={username}\npassword=test-password\nrealm={realm}\n[aor]\ntype=aor\n[desk]\ntype=endpoint\ntransport=udp\nauth={profile}\naors=aor\ncontext={context}\nallow=ulaw\nallow_transfer={}\n", if allowed { "yes" } else { "no" }),
        "", "",
    ).map_err(|error| error.to_string())?;
    Ok((
        config
            .endpoints
            .into_iter()
            .next()
            .ok_or("Missing endpoint")?,
        config
            .auth
            .into_iter()
            .next()
            .ok_or("Missing authentication profile")?,
    ))
}

#[test]
fn matching_authenticated_profiles_and_contexts_grant_local_replacement() -> Result<(), String> {
    let (source, auth) = configuration("credential", "house", "phone", "internal", true)?;
    let mut consulted = source.clone();
    consulted.name = "consultation-device".into();
    consulted.allow_transfer = false;
    consulted.refer_blind_progress = false;
    let source = ReferAuthority::authenticated(&source, &auth);
    let consulted = ReferAuthority::authenticated(&consulted, &auth);
    assert_eq!(source.replacement_authority(&consulted), Ok(()));
    assert_eq!(source.authenticated_identity(), Some("phone"));
    Ok(())
}

#[test]
fn profile_realm_username_context_and_initiator_permission_are_independent_bounds(
) -> Result<(), String> {
    let (endpoint, auth) = configuration("credential", "house", "phone", "internal", true)?;
    let source = ReferAuthority::authenticated(&endpoint, &auth);
    for (profile, realm, username, context) in [
        ("different", "house", "phone", "internal"),
        ("credential", "House", "phone", "internal"),
        ("credential", "house", "Phone", "internal"),
        ("credential", "house", "phone", "outside"),
    ] {
        let (endpoint, auth) = configuration(profile, realm, username, context, true)?;
        let consulted = ReferAuthority::authenticated(&endpoint, &auth);
        assert_eq!(
            source.replacement_authority(&consulted),
            Err(ReferFailure::Forbidden)
        );
    }
    let (denied, auth) = configuration("credential", "house", "phone", "internal", false)?;
    assert_eq!(
        ReferAuthority::authenticated(&denied, &auth).replacement_authority(&source),
        Err(ReferFailure::Forbidden),
    );
    Ok(())
}

#[test]
fn configured_peers_do_not_become_authenticated_replacement_principals() -> Result<(), String> {
    let (endpoint, auth) = configuration("credential", "house", "phone", "internal", true)?;
    let authenticated = ReferAuthority::authenticated(&endpoint, &auth);
    let configured = ReferAuthority::configured_peer(&endpoint);
    assert_eq!(configured.authenticated_identity(), None);
    for (source, consulted) in [
        (&configured, &configured),
        (&configured, &authenticated),
        (&authenticated, &configured),
    ] {
        assert_eq!(
            source.replacement_authority(consulted),
            Err(ReferFailure::Forbidden)
        );
    }
    Ok(())
}

#[test]
fn realm_snapshots_use_the_actual_inbound_challenge_realm() -> Result<(), String> {
    let (source, implicit) = configuration("credential", "", "phone", "internal", true)?;
    let (consulted, explicit) = configuration("credential", "asterisk", "phone", "internal", true)?;
    assert_eq!(
        ReferAuthority::authenticated(&source, &implicit)
            .replacement_authority(&ReferAuthority::authenticated(&consulted, &explicit)),
        Ok(()),
    );
    Ok(())
}

#[test]
fn active_authority_keeps_the_starting_profile_context_and_permission() -> Result<(), String> {
    let (mut endpoint, mut auth) = configuration("credential", "house", "phone", "internal", true)?;
    let source = ReferAuthority::authenticated(&endpoint, &auth);
    let consulted = ReferAuthority::authenticated(&endpoint, &auth);
    endpoint.context = "outside".into();
    endpoint.allow_transfer = false;
    auth.name = "changed".into();
    auth.realm = "different".into();
    auth.username = "different".into();
    assert_eq!(source.context(), "internal");
    assert_eq!(source.authenticated_identity(), Some("phone"));
    assert_eq!(source.replacement_authority(&consulted), Ok(()));
    assert_eq!(
        ReferAuthority::authenticated(&endpoint, &auth).replacement_authority(&consulted),
        Err(ReferFailure::Forbidden),
    );
    let debug = format!("{source:?}");
    for private in ["credential", "house", "phone", "test-password"] {
        assert!(!debug.contains(private));
    }
    Ok(())
}
