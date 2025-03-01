use crate::{common_properties::RequestIdMapper, errors::Error, parsers::Mining};
use core::convert::TryInto;
use mining_sv2::{
    CloseChannel, NewExtendedMiningJob, NewMiningJob, OpenExtendedMiningChannel,
    OpenExtendedMiningChannelSuccess, OpenMiningChannelError, OpenStandardMiningChannel,
    OpenStandardMiningChannelSuccess, Reconnect, SetCustomMiningJob, SetCustomMiningJobError,
    SetCustomMiningJobSuccess, SetExtranoncePrefix, SetGroupChannel, SetNewPrevHash, SetTarget,
    SubmitSharesError, SubmitSharesExtended, SubmitSharesStandard, SubmitSharesSuccess,
    UpdateChannel, UpdateChannelError,
};

use crate::{
    common_properties::{IsMiningDownstream, IsMiningUpstream},
    routing_logic::{MiningRouter, MiningRoutingLogic},
    selectors::DownstreamMiningSelector,
};

use super::SendTo_;

use crate::utils::Mutex;
use std::{fmt::Debug as D, sync::Arc};

pub type SendTo<Remote> = SendTo_<Mining<'static>, Remote>;

pub enum SupportedChannelTypes {
    Standard,
    Extended,
    Group,
    // Non header only connection can support both group and extended channels.
    GroupAndExtended,
}

/// Connection-wide downtream's messages parser implemented by an upstream.
pub trait ParseDownstreamMiningMessages<
    Up: IsMiningUpstream<Self, Selector> + D,
    Selector: DownstreamMiningSelector<Self> + D,
    Router: MiningRouter<Self, Up, Selector>,
> where
    Self: IsMiningDownstream + Sized + D,
{
    fn get_channel_type(&self) -> SupportedChannelTypes;

    fn handle_message_mining(
        self_mutex: Arc<Mutex<Self>>,
        message_type: u8,
        payload: &mut [u8],
        routing_logic: MiningRoutingLogic<Self, Up, Selector, Router>,
    ) -> Result<SendTo<Up>, Error>
    where
        Self: IsMiningDownstream + Sized,
    {
        let (channel_type, is_work_selection_enabled, downstream_mining_data) = self_mutex
            .safe_lock(|self_| {
                (
                    self_.get_channel_type(),
                    self_.is_work_selection_enabled(),
                    self_.get_downstream_mining_data(),
                )
            })
            .unwrap();
        // Is fine to unwrap on safe_lock
        match (message_type, payload).try_into() {
            Ok(Mining::OpenStandardMiningChannel(mut m)) => {
                let upstream = match routing_logic {
                    MiningRoutingLogic::None => None,
                    MiningRoutingLogic::Proxy(r_logic) => {
                        let up = r_logic
                            .safe_lock(|r_logic| {
                                r_logic.on_open_standard_channel(
                                    self_mutex.clone(),
                                    &mut m,
                                    &downstream_mining_data,
                                )
                            })
                            .unwrap();
                        Some(up?)
                    }
                    // Variant just used for phantom data is ok to panic
                    MiningRoutingLogic::_P(_) => panic!(),
                };
                match channel_type {
                    SupportedChannelTypes::Standard => self_mutex
                        .safe_lock(|self_| self_.handle_open_standard_mining_channel(m, upstream))
                        .unwrap(),
                    SupportedChannelTypes::Extended => Err(Error::UnexpectedMessage),
                    SupportedChannelTypes::Group => self_mutex
                        .safe_lock(|self_| self_.handle_open_standard_mining_channel(m, upstream))
                        .unwrap(),
                    SupportedChannelTypes::GroupAndExtended => self_mutex
                        .safe_lock(|self_| self_.handle_open_standard_mining_channel(m, upstream))
                        .unwrap(),
                }
            }
            Ok(Mining::OpenExtendedMiningChannel(m)) => match channel_type {
                SupportedChannelTypes::Standard => Err(Error::UnexpectedMessage),
                SupportedChannelTypes::Extended => self_mutex
                    .safe_lock(|self_| self_.handle_open_extended_mining_channel(m))
                    .unwrap(),
                SupportedChannelTypes::Group => Err(Error::UnexpectedMessage),
                SupportedChannelTypes::GroupAndExtended => self_mutex
                    .safe_lock(|self_| self_.handle_open_extended_mining_channel(m))
                    .unwrap(),
            },
            Ok(Mining::UpdateChannel(m)) => match channel_type {
                SupportedChannelTypes::Standard => self_mutex
                    .safe_lock(|self_| self_.handle_update_channel(m))
                    .unwrap(),
                SupportedChannelTypes::Extended => self_mutex
                    .safe_lock(|self_| self_.handle_update_channel(m))
                    .unwrap(),
                SupportedChannelTypes::Group => self_mutex
                    .safe_lock(|self_| self_.handle_update_channel(m))
                    .unwrap(),
                SupportedChannelTypes::GroupAndExtended => self_mutex
                    .safe_lock(|self_| self_.handle_update_channel(m))
                    .unwrap(),
            },
            Ok(Mining::SubmitSharesStandard(m)) => match channel_type {
                SupportedChannelTypes::Standard => self_mutex
                    .safe_lock(|self_| self_.handle_submit_shares_standard(m))
                    .unwrap(),
                SupportedChannelTypes::Extended => Err(Error::UnexpectedMessage),
                SupportedChannelTypes::Group => self_mutex
                    .safe_lock(|self_| self_.handle_submit_shares_standard(m))
                    .unwrap(),
                SupportedChannelTypes::GroupAndExtended => self_mutex
                    .safe_lock(|self_| self_.handle_submit_shares_standard(m))
                    .unwrap(),
            },
            Ok(Mining::SubmitSharesExtended(m)) => match channel_type {
                SupportedChannelTypes::Standard => Err(Error::UnexpectedMessage),
                SupportedChannelTypes::Extended => self_mutex
                    .safe_lock(|self_| self_.handle_submit_shares_extended(m))
                    .unwrap(),
                SupportedChannelTypes::Group => Err(Error::UnexpectedMessage),
                SupportedChannelTypes::GroupAndExtended => self_mutex
                    .safe_lock(|self_| self_.handle_submit_shares_extended(m))
                    .unwrap(),
            },
            Ok(Mining::SetCustomMiningJob(m)) => match (channel_type, is_work_selection_enabled) {
                (SupportedChannelTypes::Extended, true) => self_mutex
                    .safe_lock(|self_| self_.handle_set_custom_mining_job(m))
                    .unwrap(),
                (SupportedChannelTypes::Group, true) => self_mutex
                    .safe_lock(|self_| self_.handle_set_custom_mining_job(m))
                    .unwrap(),
                (SupportedChannelTypes::GroupAndExtended, true) => self_mutex
                    .safe_lock(|self_| self_.handle_set_custom_mining_job(m))
                    .unwrap(),
                _ => Err(Error::UnexpectedMessage),
            },
            Ok(_) => Err(Error::UnexpectedMessage),
            Err(e) => Err(e),
        }
    }

    fn is_work_selection_enabled(&self) -> bool;

    fn handle_open_standard_mining_channel(
        &mut self,
        m: OpenStandardMiningChannel,
        up: Option<Arc<Mutex<Up>>>,
    ) -> Result<SendTo<Up>, Error>;

    fn handle_open_extended_mining_channel(
        &mut self,
        m: OpenExtendedMiningChannel,
    ) -> Result<SendTo<Up>, Error>;

    fn handle_update_channel(&mut self, m: UpdateChannel) -> Result<SendTo<Up>, Error>;

    fn handle_submit_shares_standard(
        &mut self,
        m: SubmitSharesStandard,
    ) -> Result<SendTo<Up>, Error>;

    fn handle_submit_shares_extended(
        &mut self,
        m: SubmitSharesExtended,
    ) -> Result<SendTo<Up>, Error>;

    fn handle_set_custom_mining_job(&mut self, m: SetCustomMiningJob) -> Result<SendTo<Up>, Error>;
}
/// Connection-wide upstream's messages parser implemented by a downstream.
pub trait ParseUpstreamMiningMessages<
    Down: IsMiningDownstream + D,
    Selector: DownstreamMiningSelector<Down> + D,
    Router: MiningRouter<Down, Self, Selector>,
> where
    Self: IsMiningUpstream<Down, Selector> + Sized + D,
{
    fn get_channel_type(&self) -> SupportedChannelTypes;

    fn get_request_id_mapper(&mut self) -> Option<Arc<Mutex<RequestIdMapper>>> {
        None
    }

    /// Proxies likely would want to update a downstream req id to a new one as req id must be
    /// connection-wide unique
    /// The implementor of DownstreamMining need to pass a RequestIdMapper if want to change the req id
    fn handle_message_mining(
        self_mutex: Arc<Mutex<Self>>,
        message_type: u8,
        payload: &mut [u8],
        routing_logic: MiningRoutingLogic<Down, Self, Selector, Router>,
    ) -> Result<SendTo<Down>, Error> {
        // Is fine to unwrap on safe_lock
        let (channel_type, is_work_selection_enabled) = self_mutex
            .safe_lock(|s| (s.get_channel_type(), s.is_work_selection_enabled()))
            .unwrap();

        // Is fine to unwrap on safe_lock
        match (message_type, payload).try_into() {
            Ok(Mining::OpenStandardMiningChannelSuccess(mut m)) => {
                let remote = match routing_logic {
                    MiningRoutingLogic::None => None,
                    MiningRoutingLogic::Proxy(r_logic) => {
                        let up = r_logic
                            .safe_lock(|r_logic| {
                                r_logic.on_open_standard_channel_success(self_mutex.clone(), &mut m)
                            })
                            .unwrap();
                        Some(up?)
                    }
                    // Variant just used for phantom data is ok to panic
                    MiningRoutingLogic::_P(_) => panic!(),
                };
                match channel_type {
                    SupportedChannelTypes::Standard => self_mutex
                        .safe_lock(|s| s.handle_open_standard_mining_channel_success(m, remote))
                        .unwrap(),
                    SupportedChannelTypes::Extended => Err(Error::UnexpectedMessage),
                    SupportedChannelTypes::Group => self_mutex
                        .safe_lock(|s| s.handle_open_standard_mining_channel_success(m, remote))
                        .unwrap(),
                    SupportedChannelTypes::GroupAndExtended => self_mutex
                        .safe_lock(|s| s.handle_open_standard_mining_channel_success(m, remote))
                        .unwrap(),
                }
            }
            Ok(Mining::OpenExtendedMiningChannelSuccess(m)) => match channel_type {
                SupportedChannelTypes::Standard => Err(Error::UnexpectedMessage),
                SupportedChannelTypes::Extended => self_mutex
                    .safe_lock(|s| s.handle_open_extended_mining_channel_success(m))
                    .unwrap(),
                SupportedChannelTypes::Group => Err(Error::UnexpectedMessage),
                SupportedChannelTypes::GroupAndExtended => self_mutex
                    .safe_lock(|s| s.handle_open_extended_mining_channel_success(m))
                    .unwrap(),
            },
            Ok(Mining::OpenMiningChannelError(m)) => match channel_type {
                SupportedChannelTypes::Standard => self_mutex
                    .safe_lock(|x| x.handle_open_mining_channel_error(m))
                    .unwrap(),
                SupportedChannelTypes::Extended => self_mutex
                    .safe_lock(|x| x.handle_open_mining_channel_error(m))
                    .unwrap(),
                SupportedChannelTypes::Group => self_mutex
                    .safe_lock(|x| x.handle_open_mining_channel_error(m))
                    .unwrap(),
                SupportedChannelTypes::GroupAndExtended => self_mutex
                    .safe_lock(|x| x.handle_open_mining_channel_error(m))
                    .unwrap(),
            },
            Ok(Mining::UpdateChannelError(m)) => match channel_type {
                SupportedChannelTypes::Standard => self_mutex
                    .safe_lock(|x| x.handle_update_channel_error(m))
                    .unwrap(),
                SupportedChannelTypes::Extended => self_mutex
                    .safe_lock(|x| x.handle_update_channel_error(m))
                    .unwrap(),
                SupportedChannelTypes::Group => self_mutex
                    .safe_lock(|x| x.handle_update_channel_error(m))
                    .unwrap(),
                SupportedChannelTypes::GroupAndExtended => self_mutex
                    .safe_lock(|x| x.handle_update_channel_error(m))
                    .unwrap(),
            },
            Ok(Mining::CloseChannel(m)) => match channel_type {
                SupportedChannelTypes::Standard => {
                    self_mutex.safe_lock(|x| x.handle_close_channel(m)).unwrap()
                }
                SupportedChannelTypes::Extended => {
                    self_mutex.safe_lock(|x| x.handle_close_channel(m)).unwrap()
                }
                SupportedChannelTypes::Group => {
                    self_mutex.safe_lock(|x| x.handle_close_channel(m)).unwrap()
                }
                SupportedChannelTypes::GroupAndExtended => {
                    self_mutex.safe_lock(|x| x.handle_close_channel(m)).unwrap()
                }
            },
            Ok(Mining::SetExtranoncePrefix(m)) => match channel_type {
                SupportedChannelTypes::Standard => self_mutex
                    .safe_lock(|x| x.handle_set_extranonce_prefix(m))
                    .unwrap(),
                SupportedChannelTypes::Extended => self_mutex
                    .safe_lock(|x| x.handle_set_extranonce_prefix(m))
                    .unwrap(),
                SupportedChannelTypes::Group => self_mutex
                    .safe_lock(|x| x.handle_set_extranonce_prefix(m))
                    .unwrap(),
                SupportedChannelTypes::GroupAndExtended => self_mutex
                    .safe_lock(|x| x.handle_set_extranonce_prefix(m))
                    .unwrap(),
            },
            Ok(Mining::SubmitSharesSuccess(m)) => match channel_type {
                SupportedChannelTypes::Standard => self_mutex
                    .safe_lock(|x| x.handle_submit_shares_success(m))
                    .unwrap(),
                SupportedChannelTypes::Extended => self_mutex
                    .safe_lock(|x| x.handle_submit_shares_success(m))
                    .unwrap(),
                SupportedChannelTypes::Group => self_mutex
                    .safe_lock(|x| x.handle_submit_shares_success(m))
                    .unwrap(),
                SupportedChannelTypes::GroupAndExtended => self_mutex
                    .safe_lock(|x| x.handle_submit_shares_success(m))
                    .unwrap(),
            },
            Ok(Mining::SubmitSharesError(m)) => match channel_type {
                SupportedChannelTypes::Standard => self_mutex
                    .safe_lock(|x| x.handle_submit_shares_error(m))
                    .unwrap(),
                SupportedChannelTypes::Extended => self_mutex
                    .safe_lock(|x| x.handle_submit_shares_error(m))
                    .unwrap(),
                SupportedChannelTypes::Group => self_mutex
                    .safe_lock(|x| x.handle_submit_shares_error(m))
                    .unwrap(),
                SupportedChannelTypes::GroupAndExtended => self_mutex
                    .safe_lock(|x| x.handle_submit_shares_error(m))
                    .unwrap(),
            },
            Ok(Mining::NewMiningJob(m)) => match channel_type {
                SupportedChannelTypes::Standard => self_mutex
                    .safe_lock(|x| x.handle_new_mining_job(m))
                    .unwrap(),
                SupportedChannelTypes::Extended => Err(Error::UnexpectedMessage),
                SupportedChannelTypes::Group => Err(Error::UnexpectedMessage),
                SupportedChannelTypes::GroupAndExtended => Err(Error::UnexpectedMessage),
            },
            Ok(Mining::NewExtendedMiningJob(m)) => match channel_type {
                SupportedChannelTypes::Standard => Err(Error::UnexpectedMessage),
                SupportedChannelTypes::Extended => self_mutex
                    .safe_lock(|x| x.handle_new_extended_mining_job(m))
                    .unwrap(),
                SupportedChannelTypes::Group => self_mutex
                    .safe_lock(|x| x.handle_new_extended_mining_job(m))
                    .unwrap(),
                SupportedChannelTypes::GroupAndExtended => self_mutex
                    .safe_lock(|x| x.handle_new_extended_mining_job(m))
                    .unwrap(),
            },
            Ok(Mining::SetNewPrevHash(m)) => match channel_type {
                SupportedChannelTypes::Standard => self_mutex
                    .safe_lock(|x| x.handle_set_new_prev_hash(m))
                    .unwrap(),
                SupportedChannelTypes::Extended => self_mutex
                    .safe_lock(|x| x.handle_set_new_prev_hash(m))
                    .unwrap(),
                SupportedChannelTypes::Group => self_mutex
                    .safe_lock(|x| x.handle_set_new_prev_hash(m))
                    .unwrap(),
                SupportedChannelTypes::GroupAndExtended => self_mutex
                    .safe_lock(|x| x.handle_set_new_prev_hash(m))
                    .unwrap(),
            },
            Ok(Mining::SetCustomMiningJobSuccess(m)) => {
                match (channel_type, is_work_selection_enabled) {
                    (SupportedChannelTypes::Extended, true) => self_mutex
                        .safe_lock(|x| x.handle_set_custom_mining_job_success(m))
                        .unwrap(),
                    (SupportedChannelTypes::Group, true) => self_mutex
                        .safe_lock(|x| x.handle_set_custom_mining_job_success(m))
                        .unwrap(),
                    (SupportedChannelTypes::GroupAndExtended, true) => self_mutex
                        .safe_lock(|x| x.handle_set_custom_mining_job_success(m))
                        .unwrap(),
                    _ => Err(Error::UnexpectedMessage),
                }
            }
            Ok(Mining::SetCustomMiningJobError(m)) => {
                match (channel_type, is_work_selection_enabled) {
                    (SupportedChannelTypes::Extended, true) => self_mutex
                        .safe_lock(|x| x.handle_set_custom_mining_job_error(m))
                        .unwrap(),
                    (SupportedChannelTypes::Group, true) => self_mutex
                        .safe_lock(|x| x.handle_set_custom_mining_job_error(m))
                        .unwrap(),
                    (SupportedChannelTypes::GroupAndExtended, true) => self_mutex
                        .safe_lock(|x| x.handle_set_custom_mining_job_error(m))
                        .unwrap(),
                    _ => Err(Error::UnexpectedMessage),
                }
            }
            Ok(Mining::SetTarget(m)) => match channel_type {
                SupportedChannelTypes::Standard => {
                    self_mutex.safe_lock(|x| x.handle_set_target(m)).unwrap()
                }
                SupportedChannelTypes::Extended => {
                    self_mutex.safe_lock(|x| x.handle_set_target(m)).unwrap()
                }
                SupportedChannelTypes::Group => {
                    self_mutex.safe_lock(|x| x.handle_set_target(m)).unwrap()
                }
                SupportedChannelTypes::GroupAndExtended => {
                    self_mutex.safe_lock(|x| x.handle_set_target(m)).unwrap()
                }
            },
            Ok(Mining::Reconnect(m)) => match channel_type {
                SupportedChannelTypes::Standard => {
                    self_mutex.safe_lock(|x| x.handle_reconnect(m)).unwrap()
                }
                SupportedChannelTypes::Extended => {
                    self_mutex.safe_lock(|x| x.handle_reconnect(m)).unwrap()
                }
                SupportedChannelTypes::Group => {
                    self_mutex.safe_lock(|x| x.handle_reconnect(m)).unwrap()
                }
                SupportedChannelTypes::GroupAndExtended => {
                    self_mutex.safe_lock(|x| x.handle_reconnect(m)).unwrap()
                }
            },
            Ok(Mining::SetGroupChannel(m)) => match channel_type {
                SupportedChannelTypes::Standard => Err(Error::UnexpectedMessage),
                SupportedChannelTypes::Extended => Err(Error::UnexpectedMessage),
                SupportedChannelTypes::Group => self_mutex
                    .safe_lock(|x| x.handle_set_group_channel(m))
                    .unwrap(),
                SupportedChannelTypes::GroupAndExtended => self_mutex
                    .safe_lock(|x| x.handle_set_group_channel(m))
                    .unwrap(),
            },
            Ok(_) => Err(Error::UnexpectedMessage),
            Err(e) => Err(e),
        }
    }

    fn is_work_selection_enabled(&self) -> bool;

    fn handle_open_standard_mining_channel_success(
        &mut self,
        m: OpenStandardMiningChannelSuccess,
        remote: Option<Arc<Mutex<Down>>>,
    ) -> Result<SendTo<Down>, Error>;

    fn handle_open_extended_mining_channel_success(
        &mut self,
        m: OpenExtendedMiningChannelSuccess,
    ) -> Result<SendTo<Down>, Error>;

    fn handle_open_mining_channel_error(
        &mut self,
        m: OpenMiningChannelError,
    ) -> Result<SendTo<Down>, Error>;

    fn handle_update_channel_error(&mut self, m: UpdateChannelError)
        -> Result<SendTo<Down>, Error>;

    fn handle_close_channel(&mut self, m: CloseChannel) -> Result<SendTo<Down>, Error>;

    fn handle_set_extranonce_prefix(
        &mut self,
        m: SetExtranoncePrefix,
    ) -> Result<SendTo<Down>, Error>;

    fn handle_submit_shares_success(
        &mut self,
        m: SubmitSharesSuccess,
    ) -> Result<SendTo<Down>, Error>;

    fn handle_submit_shares_error(&mut self, m: SubmitSharesError) -> Result<SendTo<Down>, Error>;

    fn handle_new_mining_job(&mut self, m: NewMiningJob) -> Result<SendTo<Down>, Error>;

    fn handle_new_extended_mining_job(
        &mut self,
        m: NewExtendedMiningJob,
    ) -> Result<SendTo<Down>, Error>;

    fn handle_set_new_prev_hash(&mut self, m: SetNewPrevHash) -> Result<SendTo<Down>, Error>;

    fn handle_set_custom_mining_job_success(
        &mut self,
        m: SetCustomMiningJobSuccess,
    ) -> Result<SendTo<Down>, Error>;

    fn handle_set_custom_mining_job_error(
        &mut self,
        m: SetCustomMiningJobError,
    ) -> Result<SendTo<Down>, Error>;

    fn handle_set_target(&mut self, m: SetTarget) -> Result<SendTo<Down>, Error>;

    fn handle_reconnect(&mut self, m: Reconnect) -> Result<SendTo<Down>, Error>;

    fn handle_set_group_channel(&mut self, _m: SetGroupChannel) -> Result<SendTo<Down>, Error> {
        Ok(SendTo::None(None))
    }
}
