use crate::com::api::*;
use futures::stream::Stream;
use futures::Future;
use reqwest::header::{HeaderMap, HeaderName};
use reqwest::r#async::{Client as InnerClient, ClientBuilder, Decoder};
use std::collections::HashMap;
use std::mem;
use std::sync::Arc;
use std::time::Duration;
use url::form_urlencoded::byte_serialize;
use url::Url;

/// A client for communicating with Pool/Proxy/Wallet.
#[derive(Clone, Debug)]
struct Client {
    inner: InnerClient,
    account_id_to_secret_phrase: Arc<HashMap<u64, String>>,
    base_uri: Url,
    total_size_gb: usize,
    headers: Arc<HeaderMap>,
}

/// Parameters ussed for nonce submission.
#[derive(Clone, PartialEq, Debug)]
pub struct SubmissionParameters {
    account_id: u64,
    nonce: u64,
    height: u64,
    deadline_unadjusted: u64,
    deadline: u64,
}

/// Whether to send additional data for Proxies.
#[derive(Clone, PartialEq, Debug)]
pub enum ProxyDetails {
    /// Send additional data like capacity, miner name, ...
    Enabled,
    /// Don't send any additional data:
    Disabled,
}

impl Client {
    fn ua() -> String {
        "Scavenger/".to_owned() + crate_version!()
    }

    fn submit_nonce_headers(
        proxy_details: ProxyDetails,
        total_size_gb: usize,
        additional_headers: HashMap<String, String>,
    ) -> HeaderMap {
        let ua = Client::ua();
        let mut headers = HeaderMap::new();
        headers.insert("User-Agent", ua.to_owned().parse().unwrap());
        if proxy_details == ProxyDetails::Enabled {
            // It's amazing how a user agent is just not enough.
            headers.insert("X-Capacity", total_size_gb.to_string().parse().unwrap());
            headers.insert("X-Miner", ua.to_owned().parse().unwrap());
            headers.insert(
                "X-Minername",
                hostname::get_hostname()
                    .unwrap_or_else(|| "".to_owned())
                    .parse()
                    .unwrap(),
            );
            headers.insert(
                "X-Plotfile",
                ("ScavengerProxy/".to_owned()
                    + &*hostname::get_hostname().unwrap_or_else(|| "".to_owned()))
                    .parse()
                    .unwrap(),
            );
        }

        for (key, value) in additional_headers {
            let header_name = HeaderName::from_bytes(&key.into_bytes()).unwrap();
            headers.insert(header_name, value.parse().unwrap());
        }

        headers
    }

    /// Create a new client communicating with Pool/Proxy/Wallet.
    pub fn new(
        base_uri: Url,
        mut secret_phrases: HashMap<u64, String>,
        timeout: u64,
        total_size_gb: usize,
        proxy_details: ProxyDetails,
        additional_headers: HashMap<String, String>,
    ) -> Self {
        for secret_phrase in secret_phrases.values_mut() {
            *secret_phrase = byte_serialize(secret_phrase.as_bytes()).collect();
        }

        let headers =
            Client::submit_nonce_headers(proxy_details, total_size_gb, additional_headers);

        let client = ClientBuilder::new()
            .timeout(Duration::from_millis(timeout))
            .build()
            .unwrap();

        Self {
            inner: client,
            account_id_to_secret_phrase: Arc::new(secret_phrases),
            base_uri,
            total_size_gb,
            headers: Arc::new(headers),
        }
    }

    /// Get current mining info.
    pub fn get_mining_info(&self) -> impl Future<Item = MiningInfoResponse, Error = FetchError> {
        self.inner
            .get(self.base_uri.join("/burst").unwrap())
            .query(&GetMiningInfoRequest {
                request_type: &"getMiningInfo",
            })
            .send()
            .and_then(|mut res| {
                let body = mem::replace(res.body_mut(), Decoder::empty());
                body.concat2()
            })
            .from_err::<FetchError>()
            .and_then(|body| match parse_json_result(&body) {
                Ok(x) => Ok(x),
                Err(e) => Err(e.into()),
            })
    }

    /// Submit nonce to the pool and get the corresponding deadline.
    pub fn submit_nonce(
        &self,
        submission_data: &SubmissionParameters,
    ) -> impl Future<Item = SubmitNonceResonse, Error = FetchError> {
        let secret_phrase = self
            .account_id_to_secret_phrase
            .get(&submission_data.account_id);

        // If we don't have a secret phrase then we most likely talk to a pool or a proxy.
        // Both can make use of the deadline, e.g. a proxy won't validate deadlines but still
        // needs to rank the deadlines.
        // The best thing is that legacy proxies use the unadjusted deadlines so...
        // yay another parameter!
        let deadline = if secret_phrase.is_none() {
            Some(submission_data.deadline_unadjusted)
        } else {
            None
        };

        let query = SubmitNonceRequest {
            request_type: &"submitNonce",
            account_id: submission_data.account_id,
            nonce: submission_data.nonce,
            secret_phrase,
            blockheight: submission_data.height,
            deadline,
        };

        // Some "Extrawurst" for the CreepMiner proxy (I think?) which needs the deadline inside
        // the "X-Deadline" header.
        let mut headers = (*self.headers).clone();
        headers.insert(
            "X-Deadline",
            submission_data.deadline.to_string().parse().unwrap(),
        );

        self.inner
            .post(self.base_uri.join("/burst").unwrap())
            .headers(headers)
            .query(&query)
            .send()
            .and_then(|mut res| {
                let body = mem::replace(res.body_mut(), Decoder::empty());
                body.concat2()
            })
            .from_err::<FetchError>()
            .and_then(|body| match parse_json_result(&body) {
                Ok(x) => Ok(x),
                Err(e) => Err(e.into()),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio;

    static BASE_URL: &str = "http://94.130.178.37:31000";

    #[test]
    fn test_get_mining_info() {
        let mut rt = tokio::runtime::Runtime::new().expect("can't create runtime");

        let client = Client::new(
            BASE_URL.parse().unwrap(),
            HashMap::new(),
            3,
            12,
            ProxyDetails::Enabled,
            HashMap::new(),
        );

        if let Err(e) = rt.block_on(client.get_mining_info()) {
            assert!(false, format!("can't get mining info: {:?}", e));
        }
    }

    #[test]
    fn test_submit_nonce() {
        let mut rt = tokio::runtime::Runtime::new().expect("can't create runtime");

        let client = Client::new(
            BASE_URL.parse().unwrap(),
            HashMap::new(),
            3,
            12,
            ProxyDetails::Enabled,
            HashMap::new(),
        );

        let nonce_submission_response = rt.block_on(client.submit_nonce(&SubmissionParameters {
            account_id: 1337,
            nonce: 12,
            height: 112,
            deadline_unadjusted: 7123,
            deadline: 1193,
        }));

        if let Err(e) = nonce_submission_response {
            assert!(false, format!("can't submit nonce: {:?}", e));
        }
    }
}