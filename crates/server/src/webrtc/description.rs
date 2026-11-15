use rusterisk_sip::WebRtcDescription;

pub(super) fn for_transport(description: &WebRtcDescription) -> String {
    let source = description.as_str();
    let security = ["a=ice-ufrag:", "a=ice-pwd:", "a=fingerprint:", "a=setup:"];
    let mut effective = String::with_capacity(source.len());
    let mut in_media = false;
    for line in source.lines() {
        if line.starts_with("m=") {
            in_media = true;
        }
        if !in_media && security.iter().any(|prefix| line.starts_with(*prefix)) {
            continue;
        }
        effective.push_str(line);
        effective.push_str("\r\n");
    }
    for prefix in security {
        if source
            .lines()
            .skip_while(|line| !line.starts_with("m="))
            .any(|line| line.starts_with(prefix))
        {
            continue;
        }
        for line in source
            .lines()
            .take_while(|line| !line.starts_with("m="))
            .filter(|line| line.starts_with(prefix))
        {
            effective.push_str(line);
            effective.push_str("\r\n");
        }
    }
    effective
}
