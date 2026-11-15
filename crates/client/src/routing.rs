use crate::{api, Client, Error};

#[derive(serde::Serialize)]
struct MoveRequest<'a> {
    app: &'a api::ChannelId,
    #[serde(rename = "appArgs")]
    arguments: String,
}

impl Client {
    pub async fn move_channel(
        &self,
        channel: &api::ChannelId,
        application: &api::ChannelId,
        arguments: &[String],
    ) -> Result<(), Error> {
        if arguments.len() > 32
            || arguments.iter().any(|argument| {
                argument.is_empty()
                    || argument.len() > 256
                    || argument.contains(',')
                    || argument.chars().any(char::is_control)
            })
        {
            return Err(Error::Invalid("invalid Stasis arguments"));
        }
        let arguments = arguments.join(",");
        if arguments.len() > 4096 {
            return Err(Error::Capacity);
        }
        self.request_body(
            "channels.move",
            &[("channelId", channel.as_str())],
            &[],
            &MoveRequest {
                app: application,
                arguments,
            },
        )
        .await
    }
}
