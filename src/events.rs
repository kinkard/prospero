use serenity::{
    async_trait,
    client::{Context, EventHandler},
    model::gateway::Ready,
};

pub(crate) struct Handler;

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, _: Context, ready: Ready) {
        println!("{} is connected!", ready.user.name);
    }

    // todo: leave once all users left the voice channel
    // async fn voice_state_update(&self, ctx: Context, old: Option<VoiceState>, new: VoiceState) {}
}
