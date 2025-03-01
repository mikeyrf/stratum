use super::upstream_mining::{JobDispatcher, StdFrame as UpstreamFrame, UpstreamMiningNode};
use async_channel::{Receiver, SendError, Sender};
use roles_logic_sv2::{
    common_messages_sv2::{SetupConnection, SetupConnectionSuccess},
    common_properties::{
        CommonDownstreamData, DownstreamChannel, IsDownstream, IsMiningDownstream,
    },
    errors::Error,
    handlers::{
        common::{ParseDownstreamCommonMessages, SendTo as SendToCommon},
        mining::{ParseDownstreamMiningMessages, SendTo, SupportedChannelTypes},
    },
    mining_sv2::*,
    parsers::{Mining, MiningDeviceMessages, PoolMessages},
    routing_logic::MiningProxyRoutingLogic,
    utils::Mutex,
};
use std::collections::HashMap;

use codec_sv2::{Frame, StandardEitherFrame, StandardSv2Frame};

pub type Message = MiningDeviceMessages<'static>;
pub type StdFrame = StandardSv2Frame<Message>;
pub type EitherFrame = StandardEitherFrame<Message>;

/// 1 to 1 connection with a downstream node that implement the mining (sub)protocol can be either
/// a mining device or a downstream proxy.
#[derive(Debug)]
pub struct DownstreamMiningNode {
    receiver: Receiver<EitherFrame>,
    sender: Sender<EitherFrame>,
    pub status: DownstreamMiningNodeStatus,
    // channel_id/group_id -> group_id
    channel_id_to_group_id: HashMap<u32, u32>,
    pub prev_job_id: Option<u32>,
}

#[derive(Debug)]
pub enum DownstreamMiningNodeStatus {
    Initializing,
    Paired((CommonDownstreamData, HashMap<u32, Vec<DownstreamChannel>>)),
}

impl DownstreamMiningNodeStatus {
    fn is_paired(&self) -> bool {
        match self {
            DownstreamMiningNodeStatus::Initializing => false,
            DownstreamMiningNodeStatus::Paired(_) => true,
        }
    }

    fn pair(&mut self, data: CommonDownstreamData) {
        match self {
            DownstreamMiningNodeStatus::Initializing => {
                let self_ = Self::Paired((data, HashMap::new()));
                let _ = std::mem::replace(self, self_);
            }
            DownstreamMiningNodeStatus::Paired(_) => panic!(),
        }
    }

    pub fn get_channels(&mut self) -> &mut HashMap<u32, Vec<DownstreamChannel>> {
        match self {
            DownstreamMiningNodeStatus::Initializing => panic!(),
            DownstreamMiningNodeStatus::Paired((_, channels)) => channels,
        }
    }

    fn add_channel(&mut self, channel: DownstreamChannel) {
        match self {
            DownstreamMiningNodeStatus::Initializing => panic!(),
            DownstreamMiningNodeStatus::Paired((_, channels)) => {
                match channels.get_mut(&channel.group_id()) {
                    Some(g) => g.push(channel),
                    None => {
                        channels.insert(channel.group_id(), vec![channel]);
                    }
                };
            }
        }
    }
}

use async_std::{sync::Arc, task};
use core::convert::TryInto;

impl DownstreamMiningNode {
    pub fn add_channel(&mut self, channel: DownstreamChannel) {
        self.channel_id_to_group_id
            .insert(channel.channel_id(), channel.group_id());
        self.status.add_channel(channel);
    }

    pub fn new(receiver: Receiver<EitherFrame>, sender: Sender<EitherFrame>) -> Self {
        Self {
            receiver,
            sender,
            status: DownstreamMiningNodeStatus::Initializing,
            channel_id_to_group_id: HashMap::new(),
            prev_job_id: None,
        }
    }

    /// Send SetupConnectionSuccess to donwstream and start processing new messages coming from
    /// downstream
    pub async fn start(
        self_mutex: Arc<Mutex<Self>>,
        setup_connection_success: SetupConnectionSuccess,
    ) {
        if self_mutex
            .safe_lock(|self_| self_.status.is_paired())
            .unwrap()
        {
            let setup_connection_success: MiningDeviceMessages = setup_connection_success.into();

            {
                DownstreamMiningNode::send(
                    self_mutex.clone(),
                    setup_connection_success.try_into().unwrap(),
                )
                .await
                .unwrap();
            }

            task::spawn(async move {
                loop {
                    let receiver = self_mutex
                        .safe_lock(|self_| self_.receiver.clone())
                        .unwrap();
                    let message = receiver.recv().await.unwrap();
                    let incoming: StdFrame = message.try_into().unwrap();
                    Self::next(self_mutex.clone(), incoming).await
                }
            })
            .await;
        } else {
            panic!()
        }
    }

    /// Parse the received message and relay it to the right upstream
    pub async fn next(self_mutex: Arc<Mutex<Self>>, mut incoming: StdFrame) {
        let message_type = incoming.get_header().unwrap().msg_type();
        let payload = incoming.payload();

        let routing_logic = crate::get_routing_logic();

        let next_message_to_send = ParseDownstreamMiningMessages::handle_message_mining(
            self_mutex.clone(),
            message_type,
            payload,
            routing_logic,
        );

        match next_message_to_send {
            Ok(SendTo::RelaySameMessage(upstream_mutex)) => {
                let sv2_frame: codec_sv2::Sv2Frame<PoolMessages, Vec<u8>> =
                    incoming.map(|payload| payload.try_into().unwrap());
                UpstreamMiningNode::send(upstream_mutex.clone(), sv2_frame)
                    .await
                    .unwrap();
            }
            Ok(SendTo::RelayNewMessage(upstream_mutex, message)) => {
                let message = PoolMessages::Mining(message);
                let frame: UpstreamFrame = message.try_into().unwrap();
                UpstreamMiningNode::send(upstream_mutex.clone(), frame)
                    .await
                    .unwrap();
            }
            Ok(SendTo::Respond(message)) => {
                let message = MiningDeviceMessages::Mining(message);
                let frame: StdFrame = message.try_into().unwrap();
                DownstreamMiningNode::send(self_mutex.clone(), frame)
                    .await
                    .unwrap();
            }
            Ok(SendTo::Multiple(_sends_to)) => {
                todo!();
            }
            Ok(SendTo::None(_)) => (),
            Err(Error::UnexpectedMessage) => todo!("148"),
            Err(_) => todo!("149"),
        }
    }

    /// Send a message downstream
    pub async fn send(
        self_mutex: Arc<Mutex<Self>>,
        sv2_frame: StdFrame,
    ) -> Result<(), SendError<StdFrame>> {
        let either_frame = sv2_frame.into();
        let sender = self_mutex.safe_lock(|self_| self_.sender.clone()).unwrap();
        match sender.send(either_frame).await {
            Ok(_) => Ok(()),
            Err(_) => {
                todo!("172")
            }
        }
    }
}

use super::upstream_mining::ProxyRemoteSelector;

/// It impl UpstreamMining cause the proxy act as an upstream node for the DownstreamMiningNode
impl
    ParseDownstreamMiningMessages<
        UpstreamMiningNode,
        ProxyRemoteSelector,
        MiningProxyRoutingLogic<Self, UpstreamMiningNode, ProxyRemoteSelector>,
    > for DownstreamMiningNode
{
    fn get_channel_type(&self) -> SupportedChannelTypes {
        SupportedChannelTypes::Group
    }

    fn is_work_selection_enabled(&self) -> bool {
        false
    }

    fn handle_open_standard_mining_channel(
        &mut self,
        _: OpenStandardMiningChannel,
        up: Option<Arc<Mutex<UpstreamMiningNode>>>,
    ) -> Result<SendTo<UpstreamMiningNode>, Error> {
        Ok(SendTo::RelaySameMessage(up.unwrap()))
    }

    fn handle_open_extended_mining_channel(
        &mut self,
        _: OpenExtendedMiningChannel,
    ) -> Result<SendTo<UpstreamMiningNode>, Error> {
        todo!()
    }

    fn handle_update_channel(
        &mut self,
        _: UpdateChannel,
    ) -> Result<SendTo<UpstreamMiningNode>, Error> {
        todo!()
    }

    fn handle_submit_shares_standard(
        &mut self,
        m: SubmitSharesStandard,
    ) -> Result<SendTo<UpstreamMiningNode>, Error> {
        println!("{:?}", m);
        match self.channel_id_to_group_id.get(&m.channel_id) {
            Some(group_id) => match crate::upstream_from_job_id(m.job_id) {
                Some(remote) => {
                    remote.safe_lock(|r| {
                        match r.channel_id_to_job_dispatcher.get(group_id) {
                            Some(JobDispatcher::Group(dispatcher)) => {
                                match dispatcher.on_submit_shares(m) {
                                    roles_logic_sv2::job_dispatcher::SendSharesResponse::Valid(m) => {
                                        // This could just relay same message and change the
                                        // job_id as we do for request_ids
                                        let message = Mining::SubmitSharesStandard(m);
                                        Ok(SendTo::RelayNewMessage(remote.clone(),message))
                                    },
                                    roles_logic_sv2::job_dispatcher::SendSharesResponse::Invalid(m) => {
                                        let message = Mining::SubmitSharesError(m);
                                        Ok(SendTo::Respond(message))
                                    }
                                }
                            },
                            Some(_) => todo!(),
                            None => todo!(),
                        }
                    }).unwrap()
                }
                None => todo!(),
            },
            None => todo!(),
        }
    }

    fn handle_submit_shares_extended(
        &mut self,
        _: SubmitSharesExtended,
    ) -> Result<SendTo<UpstreamMiningNode>, Error> {
        todo!()
    }

    fn handle_set_custom_mining_job(
        &mut self,
        _: SetCustomMiningJob,
    ) -> Result<SendTo<UpstreamMiningNode>, Error> {
        todo!()
    }
}

impl
    ParseDownstreamCommonMessages<
        MiningProxyRoutingLogic<Self, UpstreamMiningNode, ProxyRemoteSelector>,
    > for DownstreamMiningNode
{
    fn handle_setup_connection(
        &mut self,
        _: SetupConnection,
        result: Option<Result<(CommonDownstreamData, SetupConnectionSuccess), Error>>,
    ) -> Result<roles_logic_sv2::handlers::common::SendTo, Error> {
        let (data, message) = result.unwrap().unwrap();
        self.status.pair(data);
        Ok(SendToCommon::RelayNewMessage(
            Arc::new(Mutex::new(())),
            message.try_into().unwrap(),
        ))
    }
}

use async_std::{net::TcpListener, prelude::*};
use network_helpers::PlainConnection;
use std::net::SocketAddr;

pub async fn listen_for_downstream_mining(address: SocketAddr) {
    let listner = TcpListener::bind(address).await.unwrap();
    let mut incoming = listner.incoming();

    while let Some(stream) = incoming.next().await {
        let stream = stream.unwrap();
        let (receiver, sender): (Receiver<EitherFrame>, Sender<EitherFrame>) =
            PlainConnection::new(stream, 10).await;
        let node = DownstreamMiningNode::new(receiver, sender);

        task::spawn(async move {
            let mut incoming: StdFrame = node.receiver.recv().await.unwrap().try_into().unwrap();
            let message_type = incoming.get_header().unwrap().msg_type();
            let payload = incoming.payload();
            let routing_logic = crate::get_common_routing_logic();
            let node = Arc::new(Mutex::new(node));

            // Call handle_setup_connection or fail
            match DownstreamMiningNode::handle_message_common(
                node.clone(),
                message_type,
                payload,
                routing_logic,
            ) {
                Ok(SendToCommon::RelayNewMessage(_, message)) => {
                    let message = match message {
                        roles_logic_sv2::parsers::CommonMessages::SetupConnectionSuccess(m) => m,
                        _ => panic!(),
                    };
                    DownstreamMiningNode::start(node, message).await
                }
                _ => panic!(),
            }
        });
    }
}

impl IsDownstream for DownstreamMiningNode {
    fn get_downstream_mining_data(&self) -> CommonDownstreamData {
        match self.status {
            DownstreamMiningNodeStatus::Initializing => panic!(),
            DownstreamMiningNodeStatus::Paired((settings, _)) => settings,
        }
    }
}
impl IsMiningDownstream for DownstreamMiningNode {}
