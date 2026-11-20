//! Finds the PSN account ID by signing in through Sony's Remote Play login (same flow and client as chiaki-ng).

use base64::Engine;

const CLIENT_ID: &str = "ba495a24-818c-472b-b12d-ff231c1b5745";
const CLIENT_SECRET: &str = "mvaiZkRsAsI1IBkY";
const TOKEN_URL: &str = "https://auth.api.sonyentertainmentnetwork.com/2.0/oauth/token";
const REDIRECT: &str = "https://remoteplay.dl.playstation.net/remoteplay/redirect";
const SCOPE: &str = "psn:clientapp referenceDataService:countryConfig.read pushNotification:webSocket.desktop.connect sessionManager:remotePlaySession.system.update";

pub fn login_url() -> String {
    format!(
        "https://auth.api.sonyentertainmentnetwork.com/2.0/oauth/authorize?service_entity=urn:service-entity:psn&response_type=code&client_id={CLIENT_ID}&redirect_uri={REDIRECT}&scope={}&request_locale=en_US&ui=pr&service_logo=ps&layout_type=popup&smcid=remoteplay&prompt=always&PlatformPrivacyWs1=minimal&",
        SCOPE.replace(' ', "%20")
    )
}

/// Pulls `code` out of the address the browser lands on after signing in.
pub fn code_from_redirect(url: &str) -> Option<String> {
    let query = url.trim().split_once('?')?.1;
    query.split('&').find_map(|kv| kv.strip_prefix("code=")).filter(|c| !c.is_empty()).map(str::to_owned)
}

/// The account ID is the numeric PSN user ID as 8 little-endian bytes, base64-encoded.
pub fn account_id_from_user_id(user_id: &str) -> Option<String> {
    let id: u64 = user_id.trim().parse().ok()?;
    Some(base64::engine::general_purpose::STANDARD.encode(id.to_le_bytes()))
}

/// Blocking: trades the sign-in code for the account ID. Run it off the UI thread.
pub fn fetch_account_id(code: &str) -> Result<String, String> {
    let auth = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{CLIENT_ID}:{CLIENT_SECRET}")));
    let token: serde_json::Value = ureq::post(TOKEN_URL)
        .header("Authorization", &auth)
        .send_form([("grant_type", "authorization_code"), ("code", code), ("scope", SCOPE), ("redirect_uri", REDIRECT)])
        .map_err(|e| format!("Sign-in was not accepted ({e}). Sign in again and paste the new address."))?
        .body_mut()
        .read_json()
        .map_err(|e| e.to_string())?;
    let access = token["access_token"].as_str().ok_or("Sony did not return a sign-in token.")?;
    let info: serde_json::Value = ureq::get(format!("{TOKEN_URL}/{access}"))
        .header("Authorization", &auth)
        .call()
        .map_err(|e| format!("Could not read your account ({e})."))?
        .body_mut()
        .read_json()
        .map_err(|e| e.to_string())?;
    let user_id = info["user_id"].as_str().ok_or("Sony did not return your user ID.")?;
    account_id_from_user_id(user_id).ok_or_else(|| "Sony returned an unexpected user ID.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_redirect_and_encodes_id() {
        assert_eq!(code_from_redirect("https://remoteplay.dl.playstation.net/remoteplay/redirect?code=v3ABC&cid=1").as_deref(), Some("v3ABC"));
        assert_eq!(code_from_redirect("https://example.com/no-code"), None);
        assert_eq!(account_id_from_user_id("1").as_deref(), Some("AQAAAAAAAAA="));
        assert!(crate::remote::parse_account_id(&account_id_from_user_id("1234567890123456789").unwrap()).is_some());
    }
}
