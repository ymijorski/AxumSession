#[cfg(feature = "rest_mode")]
use crate::SessionError;
use crate::{config::SecurityMode, DatabasePool, Session, SessionConfig, SessionKey, SessionStore};
#[cfg(feature = "rest_mode")]
use aes_gcm::aead::{generic_array::GenericArray, Aead, AeadInPlace, KeyInit, Payload};
#[cfg(feature = "rest_mode")]
use aes_gcm::Aes256Gcm;
#[cfg(feature = "rest_mode")]
use base64::{engine::general_purpose, Engine as _};
use cookie::Key;
#[cfg(not(feature = "rest_mode"))]
use cookie::{Cookie, CookieJar};
#[cfg(not(feature = "rest_mode"))]
use http::header::{COOKIE, SET_COOKIE};
use http::{self, HeaderMap};
#[cfg(feature = "rest_mode")]
use http::{header::HeaderName, HeaderValue};
#[cfg(feature = "rest_mode")]
use rand::RngCore;
#[cfg(feature = "rest_mode")]
use std::collections::HashMap;
use std::{
    fmt::Debug,
    marker::{Send, Sync},
};
use uuid::Uuid;

// Keep these in sync, and keep the key len synced with the `private` docs as
// well as the `KEYS_INFO` const in secure::Key. from cookie-rs
#[cfg(feature = "rest_mode")]
pub(crate) const NONCE_LEN: usize = 12;
#[cfg(feature = "rest_mode")]
pub(crate) const TAG_LEN: usize = 16;
#[cfg(feature = "rest_mode")]
pub(crate) const KEY_LEN: usize = 32;

enum NameType {
    Store,
    Data,
    Key,
}

impl NameType {
    #[inline]
    pub(crate) fn get_name(&self, config: &SessionConfig) -> String {
        let name = match self {
            NameType::Data => config.session_name.to_string(),
            NameType::Store => config.store_name.to_string(),
            NameType::Key => config.key_name.to_string(),
        };

        if config.prefix_with_host {
            let mut prefixed = "__Host-".to_owned();
            prefixed.push_str(&name);
            prefixed
        } else {
            name
        }
    }
}

#[cfg(not(feature = "rest_mode"))]
pub async fn get_headers_and_key<T>(
    store: &SessionStore<T>,
    cookies: CookieJar,
) -> (SessionKey, Option<Uuid>, bool)
where
    T: DatabasePool + Clone + Debug + Sync + Send + 'static,
{
    let value = cookies
        .get_cookie(&store.config.key_name, store.config.key.as_ref())
        .and_then(|c| Uuid::parse_str(c.value()).ok());

    let session_key = match store.config.security_mode {
        SecurityMode::PerSession => SessionKey::get_or_create(store, value).await,
        SecurityMode::Simple => SessionKey::new(),
    };

    let key = match store.config.security_mode {
        SecurityMode::PerSession => Some(&session_key.key),
        SecurityMode::Simple => store.config.key.as_ref(),
    };

    let value = cookies
        .get_cookie(&store.config.session_name, key)
        .and_then(|c| Uuid::parse_str(c.value()).ok());

    let storable = cookies
        .get_cookie(&store.config.store_name, key)
        .map_or(false, |c| c.value().parse().unwrap_or(false));

    (session_key, value, storable)
}

#[cfg(feature = "rest_mode")]
pub async fn get_headers_and_key<T>(
    store: &SessionStore<T>,
    headers: HashMap<String, String>,
) -> (SessionKey, Option<Uuid>, bool)
where
    T: DatabasePool + Clone + Debug + Sync + Send + 'static,
{
    let name = store.config.key_name.to_string();
    let value = headers
        .get(&name)
        .and_then(|c| {
            if let Some(key) = &store.config.key {
                decrypt(&name, c, key).ok()
            } else {
                Some(c.to_owned())
            }
        })
        .and_then(|c| Uuid::parse_str(&c).ok());

    let session_key = match store.config.security_mode {
        SecurityMode::PerSession => SessionKey::get_or_create(store, value).await,
        SecurityMode::Simple => SessionKey::new(),
    };

    let key = match store.config.security_mode {
        SecurityMode::PerSession => Some(&session_key.key),
        SecurityMode::Simple => store.config.key.as_ref(),
    };

    let name = store.config.session_name.to_string();
    let value = headers
        .get(&name)
        .and_then(|c| {
            if let Some(key) = key {
                decrypt(&name, c, key).ok()
            } else {
                Some(c.to_owned())
            }
        })
        .and_then(|c| Uuid::parse_str(&c).ok());

    let name = store.config.store_name.to_string();
    let storable = headers
        .get(&name)
        .and_then(|c| {
            if let Some(key) = key {
                decrypt(&name, c, key).ok()
            } else {
                Some(c.to_owned())
            }
        })
        .map(|c| c.parse().unwrap_or(false));

    (session_key, value, storable.unwrap_or(false))
}

#[cfg(not(feature = "rest_mode"))]
pub(crate) trait CookiesExt {
    fn get_cookie(&self, name: &str, key: Option<&Key>) -> Option<Cookie<'static>>;
    fn add_cookie(&mut self, cookie: Cookie<'static>, key: &Option<Key>);
}

#[cfg(not(feature = "rest_mode"))]
impl CookiesExt for CookieJar {
    fn get_cookie(&self, name: &str, key: Option<&Key>) -> Option<Cookie<'static>> {
        if let Some(key) = key {
            self.private(key).get(name)
        } else {
            self.get(name).cloned()
        }
    }

    fn add_cookie(&mut self, cookie: Cookie<'static>, key: &Option<Key>) {
        if let Some(key) = key {
            self.private_mut(key).add(cookie)
        } else {
            self.add(cookie)
        }
    }
}

#[cfg(not(feature = "rest_mode"))]
fn create_cookie<'a>(config: &SessionConfig, value: String, cookie_type: NameType) -> Cookie<'a> {
    let mut cookie_builder = Cookie::build((cookie_type.get_name(config), value))
        .path(config.cookie_path.clone())
        .secure(config.cookie_secure)
        .http_only(config.cookie_http_only)
        .same_site(config.cookie_same_site);

    if let Some(domain) = &config.cookie_domain {
        cookie_builder = cookie_builder.domain(domain.clone());
    }

    if let Some(max_age) = config.cookie_max_age {
        let time_duration = max_age.to_std().expect("Max Age out of bounds");
        cookie_builder =
            cookie_builder.expires(Some((std::time::SystemTime::now() + time_duration).into()));
    }

    cookie_builder.build()
}

#[cfg(not(feature = "rest_mode"))]
fn remove_cookie<'a>(config: &SessionConfig, cookie_type: NameType) -> Cookie<'a> {
    let mut cookie_builder = Cookie::build((cookie_type.get_name(config), ""))
        .path(config.cookie_path.clone())
        .http_only(config.cookie_http_only)
        .same_site(cookie::SameSite::None);

    if let Some(domain) = &config.cookie_domain {
        cookie_builder = cookie_builder.domain(domain.clone());
    }

    if let Some(domain) = &config.cookie_domain {
        cookie_builder = cookie_builder.domain(domain.clone());
    }

    let mut cookie = cookie_builder.build();
    cookie.make_removal();
    cookie
}

#[cfg(not(feature = "rest_mode"))]
/// This will get a CookieJar from the Headers.
pub(crate) fn get_cookies(headers: &HeaderMap) -> CookieJar {
    let mut jar = CookieJar::new();

    let cookie_iter = headers
        .get_all(COOKIE)
        .into_iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|cookie| Cookie::parse_encoded(cookie.to_owned()).ok());

    for cookie in cookie_iter {
        jar.add_original(cookie);
    }

    jar
}

#[cfg(feature = "rest_mode")]
/// This will get a Hashmap of all the headers that Exist.
pub(crate) fn get_headers<T>(
    store: &SessionStore<T>,
    headers: &HeaderMap,
) -> HashMap<String, String>
where
    T: DatabasePool + Clone + Debug + Sync + Send + 'static,
{
    let mut map = HashMap::new();

    for name in [
        store.config.key_name.to_string(),
        store.config.session_name.to_string(),
        store.config.store_name.to_string(),
    ] {
        if let Some(value) = headers.get(&name) {
            if let Ok(val) = value.to_str() {
                map.insert(name, val.to_owned());
            }
        }
    }

    map
}

#[cfg(not(feature = "rest_mode"))]
fn set_cookies(jar: CookieJar, headers: &mut HeaderMap) {
    for cookie in jar.delta() {
        if let Ok(header_value) = cookie.encoded().to_string().parse() {
            headers.append(SET_COOKIE, header_value);
        }
    }
}

/// Used to Set either the Header Values or the Cookie Values.
pub(crate) fn set_headers<T>(
    session: &Session<T>,
    session_key: &SessionKey,
    headers: &mut HeaderMap,
    destroy: bool,
    storable: bool,
) where
    T: DatabasePool + Clone + Debug + Sync + Send + 'static,
{
    // Lets make a new jar as we only want to add our cookies to the Response cookie header.\
    #[cfg(not(feature = "rest_mode"))]
    {
        let mut cookies = CookieJar::new();

        // Add Per-Session encryption KeyID
        let cookie_key = match session.store.config.security_mode {
            SecurityMode::PerSession => {
                if (storable || !session.store.config.session_mode.is_opt_in()) && !destroy {
                    cookies.add_cookie(
                        create_cookie(&session.store.config, session_key.id.inner(), NameType::Key),
                        &session.store.config.key,
                    );
                } else {
                    //If not Storable we still remove the encryption key since there is no session.
                    cookies.add_cookie(
                        remove_cookie(&session.store.config, NameType::Key),
                        &session.store.config.key,
                    );
                }

                Some(session_key.key.clone())
            }
            SecurityMode::Simple => {
                cookies.add_cookie(
                    remove_cookie(&session.store.config, NameType::Key),
                    &session.store.config.key,
                );
                session.store.config.key.clone()
            }
        };

        // Add SessionID
        if (storable || !session.store.config.session_mode.is_opt_in()) && !destroy {
            cookies.add_cookie(
                create_cookie(&session.store.config, session.id.inner(), NameType::Data),
                &cookie_key,
            );
        } else {
            cookies.add_cookie(
                remove_cookie(&session.store.config, NameType::Data),
                &cookie_key,
            );
        }

        // Add Session Store Boolean
        if session.store.config.session_mode.is_opt_in() && storable && !destroy {
            cookies.add_cookie(
                create_cookie(&session.store.config, storable.to_string(), NameType::Store),
                &cookie_key,
            );
        } else {
            cookies.add_cookie(
                remove_cookie(&session.store.config, NameType::Store),
                &cookie_key,
            );
        }

        set_cookies(cookies, headers);
    }
    #[cfg(feature = "rest_mode")]
    {
        // Add Per-Session encryption KeyID
        let cookie_key = match session.store.config.security_mode {
            SecurityMode::PerSession => {
                if (storable || !session.store.config.session_mode.is_opt_in()) && !destroy {
                    let name = NameType::Key.get_name(&session.store.config);
                    let value = if let Some(key) = session.store.config.key.as_ref() {
                        encrypt(&name, &session_key.id.inner(), key)
                    } else {
                        session_key.id.inner()
                    };

                    if let Ok(name) = HeaderName::from_bytes(name.as_bytes()) {
                        if let Ok(value) = HeaderValue::from_str(&value) {
                            headers.insert(name, value);
                        }
                    }
                }

                Some(&session_key.key)
            }
            SecurityMode::Simple => session.store.config.key.as_ref(),
        };

        // Add SessionID
        if (storable || !session.store.config.session_mode.is_opt_in()) && !destroy {
            let name = NameType::Data.get_name(&session.store.config);
            let value = if let Some(key) = cookie_key {
                encrypt(&name, &session.id.inner(), key)
            } else {
                session.id.inner()
            };

            if let Ok(name) = HeaderName::from_bytes(name.as_bytes()) {
                if let Ok(value) = HeaderValue::from_str(&value) {
                    headers.insert(name, value);
                }
            }
        }

        // Add Session Store Boolean
        if session.store.config.session_mode.is_opt_in() && storable && !destroy {
            let name = NameType::Store.get_name(&session.store.config);
            let value = if let Some(key) = cookie_key {
                encrypt(&name, &storable.to_string(), key)
            } else {
                storable.to_string()
            };

            if let Ok(name) = HeaderName::from_bytes(name.as_bytes()) {
                if let Ok(value) = HeaderValue::from_str(&value) {
                    headers.insert(name, value);
                }
            }
        }
    }
}

#[cfg(feature = "rest_mode")]
///Used to encrypt the Header Values and key values
pub(crate) fn encrypt(name: &str, value: &str, key: &Key) -> String {
    let val = value.as_bytes();

    let mut data = vec![0; NONCE_LEN + val.len() + TAG_LEN];
    let (nonce, in_out) = data.split_at_mut(NONCE_LEN);
    let (in_out, tag) = in_out.split_at_mut(val.len());
    in_out.copy_from_slice(val);

    let mut rng = rand::thread_rng();
    rng.try_fill_bytes(nonce)
        .expect("couldn't random fill nonce");
    let nonce = GenericArray::clone_from_slice(nonce);

    // Use the UUID to preform actual cookie Sealing.
    let aad = name.as_bytes();
    let aead = Aes256Gcm::new(GenericArray::from_slice(key.encryption()));
    let aad_tag = aead
        .encrypt_in_place_detached(&nonce, aad, in_out)
        .expect("encryption failure!");

    tag.copy_from_slice(aad_tag.as_slice());

    general_purpose::STANDARD.encode(&data)
}

#[cfg(feature = "rest_mode")]
///Used to deencrypt the Header Values and key values.
pub(crate) fn decrypt(name: &str, value: &str, key: &Key) -> Result<String, SessionError> {
    let data = general_purpose::STANDARD.decode(value)?;
    if data.len() <= NONCE_LEN {
        return Err(SessionError::GenericNotSupportedError(
            "length of decoded data is <= NONCE_LEN".to_owned(),
        ));
    }

    let (nonce, cipher) = data.split_at(NONCE_LEN);
    let payload = Payload {
        msg: cipher,
        aad: name.as_bytes(),
    };

    let aead = Aes256Gcm::new(GenericArray::from_slice(key.encryption()));
    Ok(String::from_utf8(
        aead.decrypt(GenericArray::from_slice(nonce), payload)
            .map_err(|_| {
                SessionError::GenericNotSupportedError(
                    "invalid key/nonce/value: bad seal".to_owned(),
                )
            })?,
    )?)
}
