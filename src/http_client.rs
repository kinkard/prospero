use serenity::client::Context;

pub(crate) type HttpClient = reqwest::Client;

pub(crate) struct HttpClientKey;

impl songbird::typemap::TypeMapKey for HttpClientKey {
    type Value = HttpClient;
}

pub(crate) async fn get(ctx: &Context) -> Option<HttpClient> {
    let data = ctx.data.read().await;
    data.get::<HttpClientKey>().cloned()
}
