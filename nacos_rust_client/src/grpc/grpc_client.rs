use std::{time::Duration, collections::HashMap};

use actix::prelude::*;
use tokio_stream::StreamExt;
use tonic::transport::Channel;

use crate::grpc::{channel::CloseableChannel, api_model::ConnectionSetupRequest, utils::PayloadUtils};

use super::nacos_proto::{
    bi_request_stream_client::BiRequestStreamClient, request_client::RequestClient, Payload,
};

//type SenderType = tokio::sync::mpsc::Sender<Result<Payload, tonic::Status>>;
type ReceiverStreamType = tonic::Streaming<Payload>;
type BiStreamSenderType = tokio::sync::mpsc::Sender<Option<Payload>>;
type PayloadSenderType = tokio::sync::oneshot::Sender<Payload>;

pub struct InnerGrpcClient {
    channel: Channel,
    //bi_request_stream_client: BiRequestStreamClient<Channel>,
    //request_client: RequestClient<Channel>,
    stream_sender: Option<BiStreamSenderType>,
    stream_reader: bool,
}

impl InnerGrpcClient {
    pub fn new(addr: String) -> anyhow::Result<Self> {
        let channel = Channel::from_shared(addr)?.connect_lazy();
        Self::new_by_channel(channel)
    }

    pub fn new_by_channel(channel:Channel) -> anyhow::Result<Self> {
        //let bi_request_stream_client = BiRequestStreamClient::new(channel.clone());
        //let request_client = RequestClient::new(channel.clone());
        Ok(Self {
            channel,
            //bi_request_stream_client,
            //request_client,
            stream_sender: Default::default(),
            stream_reader: false,
        })
    }

    fn conn_bi_stream(&mut self, ctx: &mut Context<Self>) -> anyhow::Result<()> {
        let (tx, rx) = tokio::sync::mpsc::channel(5);
        let r_stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        let r_stream = CloseableChannel::new(r_stream);
        let req = tonic::Request::new(r_stream);
        let channel = self.channel.clone();
        self.stream_sender = Some(tx);
        async move {
            /* 
            let val="{}";
            let payload=PayloadUtils::build_payload("ServerCheckRequest", val.to_string());
            let  mut request_client = RequestClient::new(channel.clone());
            let response =request_client.request(tonic::Request::new(payload)).await;
            let res = response.unwrap().into_inner();
            log::info!("first check response:{}",&PayloadUtils::get_payload_string(&res));
            */

            let mut bi_request_stream_client = BiRequestStreamClient::new(channel);
            let response = bi_request_stream_client.request_bi_stream(req).await;
            response
        }
        .into_actor(self)
        .map(|response, actor, ctx| {
            match response {
                Ok(response) => {
                    let stream = response.into_inner();
                    actor.stream_reader=true;
                    //actor.bi_stream_setup(ctx);
                    actor.receive_bi_stream(ctx,stream);
                }
                Err(err) => {
                    log::error!("conn_bi_stream error,{:?}", &err);
                    ctx.stop();
                }
            };
        })
        .spawn(ctx);
        Ok(())
    }

    fn bi_stream_setup(
        &mut self,
        ctx: &mut Context<Self>,
    ) {
        let tx = self.stream_sender.clone().unwrap();
        async move {
            let mut setup_request= ConnectionSetupRequest::default();
            setup_request.labels.insert("AppName".to_owned(), "rust_nacos_client".to_owned());
            setup_request.client_version=Some("0.3.".to_owned());
            match tx.send(Some(PayloadUtils::build_full_payload("ConnectionSetupRequest", serde_json::to_string(&setup_request).unwrap()
                    ,"127.0.0.1",HashMap::new()))).await{
                Ok(_) => {},
                Err(err) => {
                    log::error!("ConnectionSetupRequest error,{}",&PayloadUtils::get_payload_string(&err.0.unwrap()));
                },
            }
            //manage.do_send(BiStreamManageCmd::ConnClose(client_id));
        }
        .into_actor(self)
        .map(|_, _, _ctx| {
        })
        .spawn(ctx);
    }

    fn receive_bi_stream(
        &mut self,
        ctx: &mut Context<Self>,
        mut receiver_stream: ReceiverStreamType,
    ) {
        let addr = ctx.address();
        async move {
            if let Some(item) = receiver_stream.next().await {
                if let Ok(payload) = item {
                    addr.do_send(InnerGrpcClientCmd::ReceiverStreamItem(payload));
                }
            }
            while let Some(item) = receiver_stream.next().await {
                if let Ok(payload) = item {
                    addr.do_send(InnerGrpcClientCmd::ReceiverStreamItem(payload));
                } else {
                    break;
                }
            }
            //manage.do_send(BiStreamManageCmd::ConnClose(client_id));
        }
        .into_actor(self)
        .map(|_, _, ctx| {
            ctx.stop();
        })
        .spawn(ctx);
    }

    fn do_request(&mut self,ctx: &mut Context<Self>,payload:Payload,sender:Option<PayloadSenderType>) {
        let channel = self.channel.clone();
        async move {
            let  mut request_client = RequestClient::new(channel);
            let response =request_client.request(tonic::Request::new(payload)).await;
            match response{
                Ok(response) => {
                    let res= response.into_inner();
                    log::info!("check response:{}",&PayloadUtils::get_payload_string(&res));
                    if let Some(sender) = sender {
                        sender.send(res).ok();
                    }
                },
                Err(err) => {
                    log::error!("do_request error, {:?}",&err);
                },
            };
        }.into_actor(self)
        .map(|_,_,_|{
        })
        .spawn(ctx);
    }

    fn check_heartbeat(&mut self, ctx: &mut Context<Self>) {
        if self.stream_reader {
            let val="{}";
            let payload=PayloadUtils::build_payload("ServerCheckRequest", val.to_string());
            self.do_request(ctx, payload, None);
        }
    }

    pub fn heartbeat(&self,ctx:&mut actix::Context<Self>) {
        ctx.run_later(Duration::from_millis(5000), |act,ctx|{
            act.check_heartbeat(ctx);
            act.heartbeat(ctx);
        });
    }
}

impl Actor for InnerGrpcClient {
    type Context = Context<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        log::info!("InnerGrpcClient started");
        self.conn_bi_stream(ctx).ok();
        self.bi_stream_setup(ctx);
        self.heartbeat(ctx);
    }
}

#[derive(Debug, Message)]
#[rtype(result = "anyhow::Result<InnerGrpcClientResult>")]
pub enum InnerGrpcClientCmd {
    ReceiverStreamItem(Payload),
    Ping,
}

pub enum InnerGrpcClientResult {
    None,
}

impl Handler<InnerGrpcClientCmd> for InnerGrpcClient {
    type Result = anyhow::Result<InnerGrpcClientResult>;

    fn handle(&mut self, msg: InnerGrpcClientCmd, _ctx: &mut Self::Context) -> Self::Result {
        match msg {
            InnerGrpcClientCmd::ReceiverStreamItem(_payload) => {
                Ok(InnerGrpcClientResult::None)
            },
            InnerGrpcClientCmd::Ping => {
                Ok(InnerGrpcClientResult::None)
            },
        }
    }
}

