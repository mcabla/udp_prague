use crate::core::{
    PragueClassicAqmEvent, PragueRecvAckEvent, PragueRecvDataEvent, PragueRecvRfc8888AckEvent,
    PragueSendAckEvent, PragueSendDataEvent, PragueSendFrameDataEvent, PragueSendRfc8888AckEvent,
    Reporter, RunnerConfig,
};

use super::AppStuff;

impl From<&AppStuff> for RunnerConfig {
    fn from(app: &AppStuff) -> Self {
        Self {
            rcv_addr: app.rcv_addr.clone(),
            rcv_port: app.rcv_port,
            connect: app.connect,
            startup_wait_timeout_us: app.startup_wait_timeout_us,
            max_pkt: app.max_pkt,
            max_rate: app.max_rate,
            rfc8888_ack: app.rfc8888_ack,
            rfc8888_ackperiod: app.rfc8888_ackperiod,
            rt_mode: app.rt_mode,
            rt_fps: app.rt_fps,
            rt_frameduration: app.rt_frameduration,
            classic_aqm_fallback_enabled: app.classic_aqm_fallback_enabled,
        }
    }
}

impl Reporter for AppStuff {
    fn LogSendData(&mut self, event: &PragueSendDataEvent) {
        AppStuff::LogSendData(self, event);
    }

    fn LogSendFrameData(&mut self, event: &PragueSendFrameDataEvent) {
        AppStuff::LogSendFrameData(self, event);
    }

    fn LogRecvData(&mut self, event: &PragueRecvDataEvent) {
        AppStuff::LogRecvData(self, event);
    }

    fn LogSendACK(&mut self, event: &PragueSendAckEvent) {
        AppStuff::LogSendACK(self, event);
    }

    fn LogSendRFC8888ACK(&mut self, event: &PragueSendRfc8888AckEvent<'_>) {
        AppStuff::LogSendRFC8888ACK(self, event);
    }

    fn LogRecvACK(&mut self, event: &PragueRecvAckEvent) {
        AppStuff::LogRecvACK(self, event);
    }

    fn LogRecvRFC8888ACK(&mut self, event: &PragueRecvRfc8888AckEvent<'_>) {
        AppStuff::LogRecvRFC8888ACK(self, event);
    }

    fn LogClassicAqm(&mut self, event: &PragueClassicAqmEvent) {
        AppStuff::LogClassicAqm(self, event);
    }
}
