use crate::MDBResponse;
use crate::MDBStatus;
use crate::Mdb;


use embedded_io_async::{Read, Write};

use defmt::Format;
use enumn::N;

use fixedstr::str16;

use embassy_time::Timer;


const RESET: u8 = 0x10;

const SETUP_PREFIX: u8 = 0x11;
const SETUP_CONFIG_DATA: u8 = 0x00;
const SETUP_MAX_MIN_PRICES: u8 = 0x01;
const SETUP_REPLY_READER_CONFIG_DATA: u8 = 0x01;

const POLL_CMD: u8 = 0x12;
//Various poll replies
const POLL_REPLY_JUST_RESET: u8 = 0x00;
const POLL_REPLY_READER_CONFIG_DATA: u8 = 0x01;
const POLL_REPLY_DISPLAY_REQUEST: u8 = 0x02;
const POLL_REPLY_BEGIN_SESSION: u8 = 0x03;
const POLL_REPLY_SESSION_CANCEL_REQUEST: u8 = 0x04;
const POLL_REPLY_VEND_APPROVED: u8 = 0x05;
const POLL_REPLY_VEND_DENIED: u8 = 0x06;
const POLL_REPLY_END_SESSION: u8 = 0x07;
const POLL_REPLY_CANCELLED: u8 = 0x08;
const POLL_REPLY_PERIPHERAL_ID: u8 = 0x09;
const POLL_REPLY_MALFUNCTION: u8 = 0x0A;
const POLL_REPLY_OUT_OF_SEQUENCE: u8 = 0x0B;

const POLL_REPLY_REVALUE_APPROVED: u8 = 0x0D;
const POLL_REPLY_REVALUE_DENIED: u8 = 0x0E;
const POLL_REPLY_REVALUE_LIMIT_AMOUNT: u8 = 0x0F;
const POLL_REPLY_USER_FILE_DATA: u8 = 0x10;
const POLL_REPLY_TIME_DATE_REQUEST: u8 = 0x11;
const POLL_REPLY_DATA_ENTRY_REQUEST: u8 = 0x12;
const POLL_REQUEST_DATA_ENTRY_CANCEL: u8 = 0x13;
//We do not support FTL
const POLL_REPLY_DIAGNOSTICS: u8 = 0xFF;

//Vend commands
const VEND_PREFIX: u8 = 0x13;
const VEND_REQUEST: u8 = 0x00;
const VEND_CANCEL: u8 = 0x01;
const VEND_SUCCESS: u8 = 0x02;
const VEND_FAILURE: u8 = 0x03;
const VEND_SESSION_COMPLETE: u8 = 0x04;
const VEND_CASH_SALE: u8 = 0x05;
const NEGATIVE_VEND_REQUEST: u8 = 0x06;
//Vend replies
const VEND_REPLY_APPROVED: u8 = 0x05;
const VEND_REPLY_DENIED: u8 = 0x06;
const VEND_REPLY_END_SESSION: u8 = 0x07;
const VEND_REPLY_CANCELLED: u8 = 0x08;

//Vend reader commands
const VEND_READER_PREFIX: u8 = 0x14;
const VEND_READER_DISABLE: u8 = 0x00;
const VEND_READER_ENABLE: u8 = 0x01;
const VEND_READER_CANCEL: u8 = 0x02;
const VEND_READER_DATA_ENTRY_RESP: u8 = 0x03;

//Vend revalue commands
const VEND_REVALUE_PREFIX: u8 = 0x15;
const VEND_REVALUE_REQUEST: u8 = 0x00;
const VEND_REVALUE_LIMIT_REQUEST: u8 = 0x01;
//Vend revalue replies
const VEND_REPLY_REVALUE_APPROVED: u8 = 0x0D;
const VEND_REPLY_REVALUE_DENIED: u8 = 0x0E;
const VEND_REPLY_REVALUE_LIMIT_AMOUNT: u8 = 0x0F;

//Some multi byte pre-written message to send to device
//Breakdown - VMC level 3, display with no rows, no columns (none which we will
//share with the contactless device, anyway!)
const VMC_SETUP_DATA: [u8; 6] = [0x11, 0x00, 0x03, 0x00, 0x00, 0x00];

//Max and min prices set as "dont know"
const VMC_MAX_MIN_PRICE_DATA: [u8; 6] = [0x11, 0x01, 0xFF, 0xFF, 0x00, 0x00];

//This is how we identify ourself to the cashless device
const VMC_EXPANSION_REQUEST_ID_DATA: [u8; 31] = [
    0x17, 0x00, b'D', b'M', b'P', //Manufacturer ID
    b'0', b'0', b'0', b'0', b'0', b'0', b'0', b'0', b'0', b'0', b'0', b'1', //Serial number
    b'0', b'0', b'0', b'0', b'0', b'0', b'0', b'0', b'0', b'0', b'0', b'1', //Model number
    b'0', b'1', //Software version
];

#[derive(Format)]
pub enum CashlessDeviceFeatureLevel {
    Level1,
    Level2,
    Level3,
}

#[derive(Format)]
pub struct CashlessDevice {
    pub feature_level: CashlessDeviceFeatureLevel,
    pub country_code: u16,
    pub scale_factor: u8,
    pub decimal_places: u8,
    pub max_response_time: u8,
    pub can_restore_funds: bool,
    pub multivend_capable: bool,
    pub has_display: bool,
    pub supports_cash_sale_cmd: bool,

    //These come back from the peripheral ID command (0x09)
    pub manufacturer_code: [u8; 3],
    pub serial_number: [u8; 12],
    pub model_number: [u8; 12],
    pub software_version: [u8; 2],

    //Level 3 features
    pub supports_ftl: bool,
    pub monetary_format_32_bit: bool,
    pub supports_multicurrency: bool,
    pub supports_negative_vend: bool,
    pub supports_data_entry: bool,
    pub supports_always_idle: bool,
}

impl CashlessDevice {
    /// Given the first byte of the poll command, this function will
    /// return its' length.  Needed in order to tokenize multiple
    /// responses to a poll command when they are chained into a single message
    pub fn poll_response_length(&self, poll_cmd: u8) -> usize {
        match poll_cmd {
            POLL_REPLY_JUST_RESET => 1,
            POLL_REPLY_READER_CONFIG_DATA => 8,
            POLL_REPLY_DISPLAY_REQUEST => 34,
            POLL_REPLY_BEGIN_SESSION => {
                match self.feature_level {
                    CashlessDeviceFeatureLevel::Level1 => 3,
                    _ => 10,
                    //Would be 17 if expanded currency mode enabled, but not supported currently
                }
            }
            POLL_REPLY_SESSION_CANCEL_REQUEST => 1,
            POLL_REPLY_VEND_APPROVED => 3, //NB would be 5 if expanded currency mode is enabled
            POLL_REPLY_VEND_DENIED => 1,
            POLL_REPLY_END_SESSION => 1,
            POLL_REPLY_CANCELLED => 1,
            POLL_REPLY_PERIPHERAL_ID => {
                match self.feature_level {
                    //Because the library identifies as L3 VMC, the L3 device will give us the option bits
                    //making its' reply 34 bytes long
                    CashlessDeviceFeatureLevel::Level3 => 34,
                    _ => 30,
                }
            }
            POLL_REPLY_MALFUNCTION => 2,
            POLL_REPLY_OUT_OF_SEQUENCE => match self.feature_level {
                CashlessDeviceFeatureLevel::Level1 => 1,
                _ => 2,
            },
            POLL_REPLY_REVALUE_APPROVED => 1,
            POLL_REPLY_REVALUE_DENIED => 1,
            POLL_REPLY_REVALUE_LIMIT_AMOUNT => 3,
            POLL_REPLY_TIME_DATE_REQUEST => 1,
            POLL_REPLY_DATA_ENTRY_REQUEST => 2,
            _ => {
                defmt::debug!("Got asked for length of unknown poll cmd {=u8}", poll_cmd);
                1
            } //Shouldn't happen
        }
    }

    pub async fn init<T: Read + Write>(bus: &mut Mdb<T>) -> Option<Self> {
        
        let mut buf: [u8; 64] = [0x00; 64];

        bus.send_data_and_confirm_ack(&[RESET]).await;
        bus.send_data(&[POLL_CMD]).await;

        if let Ok(MDBResponse::Data(len)) = bus.receive_response(&mut buf).await {
            if buf[0] != POLL_REPLY_JUST_RESET {
                defmt::debug!("Unexpected reply from cashless device post reset");
                return None;
            } else {
                defmt::debug!("Received JUST_RESET from cashless device post poll");
            }
        }
        bus.send_data(&VMC_SETUP_DATA).await;   
        if let Ok(MDBResponse::Data(len)) = bus.receive_response(&mut buf).await {
            if len != 8 {
                defmt::error!("Cashless device incorrect setup length {}", len);
                return None;
            }
        }
        else {
            defmt::error!("Cashless device failed to reply with setup data");
            return None;
        }

        //The buffer will contain the setup data.
        let feature_level = match buf[0x01] {
            0x02 => CashlessDeviceFeatureLevel::Level2,
            0x03 => CashlessDeviceFeatureLevel::Level3,
            _ => CashlessDeviceFeatureLevel::Level1,
        };

        let country_code: u16 = (buf[0x02] as u16) << 8 | buf[0x03] as u16;
        let scale_factor = buf[0x04];
        let decimal_places = buf[0x05];
        let max_response_time = buf[0x06];
        //Optional feature flags
        let can_restore_funds = buf[0x07] & 0x01 != 0;
        let multivend_capable = buf[0x07] & 0x02 != 0;
        let has_display = buf[0x07] & 0x04 != 0;
        let supports_cash_sale_cmd = buf[0x07] & 0x08 != 0;

        //Min max price data next
        bus.send_data_and_confirm_ack(&VMC_MAX_MIN_PRICE_DATA).await;

        bus.send_data(&VMC_EXPANSION_REQUEST_ID_DATA).await;; //as above
        if let Ok(MDBResponse::Data(len)) = bus.receive_response(&mut buf).await {
            if matches!(feature_level, CashlessDeviceFeatureLevel::Level3) {
                if len != 34 {
                    defmt::error!(
                        "L3 cashless device replied with wrong length expansion data ( {} )",
                        len
                    );
                    return None;
                }
            } else if len != 30 {
                //30 bytes if level 1-2
                defmt::error!(
                    "Non L3 cashless device replied with wrong length expansion data ( {} )",
                    len
                );
                return None;
            }
        }
        else {
            defmt::error!("Cashless device failed to reply with expansion request data");
            return None;
        };

        //Buffer will now contain correct length of data for parsing expansion request
        let c = CashlessDevice {
            feature_level,
            country_code,
            scale_factor,
            decimal_places,
            max_response_time,

            //Basic option flags
            can_restore_funds,
            multivend_capable,
            has_display,
            supports_cash_sale_cmd,

            //Data from the expansion request
            manufacturer_code: buf[1..4].try_into().unwrap(),
            serial_number: buf[4..16].try_into().unwrap(),
            model_number: buf[16..28].try_into().unwrap(),
            software_version: buf[28..30].try_into().unwrap(),

            //Level 3 features
            supports_ftl: buf[33] & 0x01 != 0,
            monetary_format_32_bit: buf[33] & 0x02 != 0,
            supports_multicurrency: buf[33] & 0x04 != 0,
            supports_negative_vend: buf[33] & 0x08 != 0,
            supports_data_entry: buf[33] & 0x10 != 0,
            supports_always_idle: buf[33] & 0x20 != 0,
        };
        //Enable always idle
        bus.send_data_and_confirm_ack(&[0x17, 0x04, 0x00, 0x00, 0x00, 0x20]).await;

        c.set_device_enabled(bus, true).await;

        Some(c)
    }

    pub async fn record_cash_transaction<T: Read + Write>(
        &self,
        bus: &mut Mdb<T>,
        unscaled_amount: u16,
        address: [u8; 2],
    ) -> bool {
        let amount = unscaled_amount.to_le_bytes();
        if bus.send_data_and_confirm_ack(&[
            VEND_PREFIX,
            VEND_CASH_SALE,
            amount[1],
            amount[0],
            address[0],
            address[1],
        ]).await {
            defmt::debug!("Record cash sale transaction success");
            true
        }
        else {
            defmt::debug!("Recorded cash sale transaction fail");
            false
        }
    }

    pub async fn start_transaction<T: Read + Write>(
        &self,
        bus: &mut Mdb<T>,
        unscaled_amount: u16,
        address: [u8; 2],
    ) -> bool {
        let mut buf: [u8; 64] = [0x00; 64];

        let amount = unscaled_amount.to_le_bytes();
        bus.send_data_and_confirm_ack(&[
            VEND_PREFIX,
            VEND_REQUEST,
            amount[1],
            amount[0],
            address[0],
            address[1],
        ]).await;

        //Send poll command, and wait a max of 150 cycles (30 seconds) for someone to present a card
        let mut success = false;
        for i in 0..150 {
            bus.send_data(&[POLL_CMD]).await;
            if let Ok(MDBResponse::Data(len)) = bus.receive_response(&mut buf).await  {
                match buf[0] {
                    POLL_REPLY_VEND_APPROVED => {
                        let amount: u16 = (buf[1] as u16) << 8 | buf[2] as u16;
                        defmt::debug!("Card reader approved vend - up to  {}", amount);
                        success = true;
                        break;
                    }
                    POLL_REPLY_VEND_DENIED => {
                        defmt::debug!("Card reader denied vend");
                        break;
                    }
                    POLL_REPLY_SESSION_CANCEL_REQUEST => {
                        defmt::debug!("Card reader requested end of session");
                        break;
                    }
                    _ => {
                        defmt::debug!(
                            "Unexpected reply from card reader to vend request: {=[u8]:#04x}",
                            buf[0..len]
                        );
                    }
                }
            }
            else {
                defmt::debug!("Unexpected non-response reply");
            }
          //  bus.timer.delay_ms(200);
        }
        if ! success {
            //need to end session if denied.
            self.end_session(bus).await;
        }
        success
    }

    pub async fn cancel_transaction<T: Read + Write>(
        &self,
        bus: &mut Mdb<T>
    ) -> bool {
        bus.send_data_and_confirm_ack(&[VEND_PREFIX, VEND_CANCEL]).await;


        let mut buf: [u8; 64] = [0x00; 64];
        bus.send_data(&[POLL_CMD]).await;
        if let Ok(MDBResponse::Data(count)) = bus.receive_response(&mut buf).await {
            match buf[0] {
                POLL_REPLY_VEND_DENIED => {
          //          let amount: u16 = (buf[1] as u16) << 8 | buf[2] as u16;
                    defmt::debug!("Transaction cancelled");
                    return true;
                }
                _ => {
                    defmt::debug!("Unexpected reply");
                }
            }
        }

        false
    }

    pub async fn vend_success<T: Read + Write>(
        &self,
        bus: &mut Mdb<T>, 
        address: [u8; 2]
    ) -> bool {
        bus.send_data_and_confirm_ack(&[VEND_PREFIX, VEND_SUCCESS, address[0],address[1]]).await
    }


    pub async fn vend_failed<T: Read + Write>(
        &self,
        bus: &mut Mdb<T>
    ) -> bool {
        bus.send_data_and_confirm_ack(&[VEND_PREFIX, VEND_FAILURE]).await;
        //poll should get 0x06 -vend denied.
        //then we move to end session.
        bus.send_data(&[POLL_CMD]).await;

        let mut refund_complete = false;

        //fixme
        for i in 0..100 {
            if bus.send_data_and_confirm_ack(&[POLL_CMD]).await {
                refund_complete = true;
                break;
            }
            Timer::after_millis(100).await;
        }
        
        if refund_complete {
            defmt::debug!("Refund complete");
            true
        }
        else {
            defmt::debug!("Refund FAILED - credit lost");
            false
        }
    }
    
    pub async fn end_session<T: Read + Write>(
        &self,
        bus: &mut Mdb<T>
    ) -> bool {
        let mut buf: [u8; 64] = [0x00; 64];
        bus.send_data_and_confirm_ack(&[VEND_PREFIX, VEND_SESSION_COMPLETE]).await;
        bus.send_data(&[POLL_CMD]).await;
        if let Ok(MDBResponse::Data(len)) = bus.receive_response(&mut buf).await {
            match buf[0] {
                POLL_REPLY_END_SESSION => {
                    defmt::debug!("End session");
                    return true;
                },
                _ => {
                    defmt::error!(
                        "Unexpected reply from card reader to end session: {=[u8]:#04x}",
                        buf[0..len]
                    );
                }
            }
        };
        defmt::error!("end session failed!");
        return false;
    }

    pub async fn set_device_enabled<T: Read + Write>(
        &self,
        bus: &mut Mdb<T>,
        enable: bool,
    ) -> bool {
        if enable {
            bus.send_data_and_confirm_ack(&[VEND_READER_PREFIX, VEND_READER_ENABLE]).await
        } else {
            bus.send_data_and_confirm_ack(&[VEND_READER_PREFIX, VEND_READER_DISABLE]).await
        }
    }
}