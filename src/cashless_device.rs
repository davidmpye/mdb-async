use crate::MDBResponse;
use crate::MDBStatus;
use crate::Mdb;

use embedded_io_async::{Read, Write};

use defmt::*;

use core::str::from_utf8;
use fixedstr::{str16, str4};

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
//NB infuriatingly, the number of rows and columns we specify *changes* the length of the data
//in one of the poll replies (0x02 - "Display Request" - where the number of bytes must equal rows*cols!)
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

//This enables the 'always idle' feature only
const FEATURE_FLAG_DATA: [u8; 6] = [0x17, 0x04, 0x00, 0x00, 0x00, 0x20];

#[derive(Format)]
pub enum CashlessDeviceFeatureLevel {
    Level1,
    Level2,
    Level3,
}

#[derive(Copy, Clone, Debug)]
pub struct BeginSessionAdvancedData {
    funds_available: u16,
    payment_media_id: u32,
    payment_type: u8,
    payment_data: u16,
}

#[derive(Copy, Clone, Debug)]
pub enum MalfunctionCode {
    PaymentMedia,
    InvalidPaymentMedia,
    Tamper,
    MfrErr1,
    CommsErr2,
    RequiresService,
    MfrErr2,
    ReaderFailure3,
    CommsErr3,
    Jammed,
    MfrErr,
    RefundErr,
    Unassigned,
}

//A poll event might be one of the following:
#[derive(Copy, Clone, Debug)]
pub enum PollEvent {
    JustReset,
    ReaderConfigData,
    BeginSessionLevelBasic(u16), //'scaled' funds
    BeginSessionLevelAdvanced(BeginSessionAdvancedData),
    SessionCancelRequest,
    VendApproved(u16), //unscaled amount
    VendDenied,
    EndSession,
    Cancelled,
    PeripheralId, //We are a level 3 VMC, so we will parse...
    Malfunction(MalfunctionCode),
    CmdOutOfSequence, //If VMC level 3, then there'll be an optional status byte here
    RevalueApproved,
    RevalueDenied,
    RevalueLimitAmount(u16),
    UserFileData,
    TimeDateRequest,
    DataEntryRequest,
    //Unimplemented:
    //DisplayRequest - we don't have a display
    //UserFileData (obsolete)
    //TimeDateRequest
    //?  SelectionRequest,
    //?  CouponReport,
}

pub enum PollError {
    InvalidEvent,
    UnsupportedEvent,
}

impl TryFrom<&[u8]> for PollEvent {
    type Error = PollError;
    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        match bytes[0] {
            POLL_REPLY_JUST_RESET => Ok(PollEvent::JustReset),
            POLL_REPLY_READER_CONFIG_DATA => Ok(PollEvent::ReaderConfigData),
            POLL_REPLY_DISPLAY_REQUEST => Err(PollError::UnsupportedEvent),
            POLL_REPLY_BEGIN_SESSION => {
                match bytes.len() {
                    3 => {
                        //Level 1 reader
                        Ok(PollEvent::BeginSessionLevelBasic(u16::from_le_bytes([bytes[2], bytes[1]])))
                    }
                    10 => {
                        //Level 2/3 reader
                        Ok(PollEvent::BeginSessionLevelAdvanced(
                            BeginSessionAdvancedData {
                                funds_available: u16::from_le_bytes([bytes[2], bytes[1]]),
                                payment_media_id: u32::from_le_bytes([bytes[3], bytes[4], bytes[5], bytes[6]]),
                                payment_type: bytes[7],
                                payment_data: u16::from_le_bytes([bytes[9], bytes[8]]),
                            },
                        ))
                    }
                    _ => {
                        //Unrecognised length
                        Err(PollError::InvalidEvent)
                    }
                }
            }
            POLL_REPLY_SESSION_CANCEL_REQUEST => Ok(PollEvent::SessionCancelRequest),
            POLL_REPLY_VEND_APPROVED => match bytes.len() {
                3 => {
                    Ok(PollEvent::VendApproved(u16::from_le_bytes([bytes[2], bytes[1]])))
                },
                _ => Err(PollError::InvalidEvent),
            },
            POLL_REPLY_VEND_DENIED => Ok(PollEvent::VendDenied),
            POLL_REPLY_END_SESSION => Ok(PollEvent::EndSession),
            POLL_REPLY_CANCELLED => Ok(PollEvent::Cancelled),
            POLL_REPLY_PERIPHERAL_ID => Ok(PollEvent::PeripheralId),
            POLL_REPLY_MALFUNCTION => {
                match bytes.len() {
                    2 => match bytes[1] {
                        0x00 => Ok(PollEvent::Malfunction(MalfunctionCode::PaymentMedia)),
                        0x01 => Ok(PollEvent::Malfunction(MalfunctionCode::InvalidPaymentMedia)),
                        0x02 => Ok(PollEvent::Malfunction(MalfunctionCode::Tamper)),
                        0x03 => Ok(PollEvent::Malfunction(MalfunctionCode::MfrErr1)),
                        0x04 => Ok(PollEvent::Malfunction(MalfunctionCode::CommsErr2)),
                        0x05 => Ok(PollEvent::Malfunction(MalfunctionCode::RequiresService)),
                        //0x06 is 'unassigned 2' ¯\_(ツ)_/¯
                        0x07 => Ok(PollEvent::Malfunction(MalfunctionCode::MfrErr2)),
                        0x08 => Ok(PollEvent::Malfunction(MalfunctionCode::ReaderFailure3)),
                        0x09 => Ok(PollEvent::Malfunction(MalfunctionCode::CommsErr3)),
                        0x0A => Ok(PollEvent::Malfunction(MalfunctionCode::Jammed)),
                        0x0B => Ok(PollEvent::Malfunction(MalfunctionCode::MfrErr)),
                        0x0C => Ok(PollEvent::Malfunction(MalfunctionCode::RefundErr)),
                        _ => Ok(PollEvent::Malfunction(MalfunctionCode::Unassigned)),
                    },
                    _ => Err(PollError::InvalidEvent),
                }
            }
            POLL_REPLY_OUT_OF_SEQUENCE => Ok(PollEvent::CmdOutOfSequence),
            POLL_REPLY_REVALUE_APPROVED => Ok(PollEvent::RevalueApproved),
            POLL_REPLY_USER_FILE_DATA => Ok(PollEvent::UserFileData),
            POLL_REPLY_TIME_DATE_REQUEST => Ok(PollEvent::TimeDateRequest),
            POLL_REPLY_DATA_ENTRY_REQUEST => Ok(PollEvent::DataEntryRequest),
            _ => Err(PollError::InvalidEvent),
        }
    }
}

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
    pub manufacturer_code: str4,
    pub serial_number: str16,
    pub model_number: str16,
    pub software_version: str4,

    //Level 3 features
    pub supports_ftl: bool,
    pub monetary_format_32_bit: bool,
    pub supports_multicurrency: bool,
    pub supports_negative_vend: bool,
    pub supports_data_entry: bool,
    pub supports_always_idle: bool,
    //2019 added new l3 features
    pub supports_remote_vend: bool,
    pub supports_basket: bool,
    pub supports_coupon: bool,
    pub supports_ask_begin_session: bool,
    pub supports_enhanced_item_number_information: bool,
}

impl CashlessDevice {
    /// Given the first byte of the poll command, this function will
    /// return its' length.  Needed in order to tokenize multiple
    /// responses to a poll command when they are chained into a single message
    pub fn poll_response_length(&self, poll_cmd: u8) -> Result<usize, ()> {
        match poll_cmd {
            POLL_REPLY_JUST_RESET => Ok(1),
            POLL_REPLY_READER_CONFIG_DATA => Ok(8),
            POLL_REPLY_DISPLAY_REQUEST => Ok(2), //NB If you implement a reader with rows/cols, then the
            //number of bytes here are rows*cols.  We don't have any rows/cols, so we shouldnt
            //ever receive this message....
            POLL_REPLY_BEGIN_SESSION => {
                match self.feature_level {
                    CashlessDeviceFeatureLevel::Level1 => Ok(3),
                    _ => Ok(10),
                    //Would be 17 if expanded currency mode enabled, but not supported currently
                }
            }
            POLL_REPLY_SESSION_CANCEL_REQUEST => Ok(1),
            POLL_REPLY_VEND_APPROVED => Ok(3), //NB would be 5 if expanded currency mode is enabled
            POLL_REPLY_VEND_DENIED => Ok(1),
            POLL_REPLY_END_SESSION => Ok(1),
            POLL_REPLY_CANCELLED => Ok(1),
            POLL_REPLY_PERIPHERAL_ID => {
                match self.feature_level {
                    //Because the library identifies as L3 VMC, the L3 device will give us the option bits
                    //making its' reply 34 bytes long
                    CashlessDeviceFeatureLevel::Level3 => Ok(34),
                    _ => Ok(30),
                }
            }
            POLL_REPLY_MALFUNCTION => Ok(2),
            POLL_REPLY_OUT_OF_SEQUENCE => match self.feature_level {
                CashlessDeviceFeatureLevel::Level1 => Ok(1),
                _ => Ok(2),
            },
            POLL_REPLY_REVALUE_APPROVED => Ok(1),
            POLL_REPLY_REVALUE_DENIED => Ok(1),
            POLL_REPLY_REVALUE_LIMIT_AMOUNT => Ok(3),
            POLL_REPLY_TIME_DATE_REQUEST => Ok(1),
            POLL_REPLY_DATA_ENTRY_REQUEST => Ok(2),
            _ => {
                debug!("Invalid poll event byte {=u8}", poll_cmd);
                Err(())
            }
        }
    }

    pub async fn init<T: Read + Write>(bus: &mut Mdb<T>) -> Option<Self> {
        //MDB spec insists on following init sequence for cashless devices:
        //Reset
        //Poll - should reply POLL_REPLY_JUST_RESET
        //Setup config data
        //Setup max/min price
        //Expansion request ID
        //Expansion enable options
        //Setup max/min price again *IF* you've enabled 32 bit/multicurrency options
        //Reader enable (if wished)

        //We do the initial poll and parse in here rather than use the main poll function, as until
        //we know the level of device we are talking to, we can't tell how long the poll subcommands are.

        //Start with initial reset
        bus.send_data_and_confirm_ack(&[RESET]).await;

        //Initial poll, should reply JUST RESET
        bus.send_data(&[POLL_CMD]).await;
        let mut buf: [u8; 64] = [0x00; 64];
        if let Ok(MDBResponse::Data(len)) = bus.receive_response(&mut buf).await {
            if buf[0] != POLL_REPLY_JUST_RESET {
                error!("Unexpected reply from cashless device post reset");
                return None;
            } else {
                debug!("Received JUST_RESET from cashless device post poll");
            }
        }

        //VMC/device config data exchange
        bus.send_data(&VMC_SETUP_DATA).await;
        if let Ok(MDBResponse::Data(len)) = bus.receive_response(&mut buf).await {
            if len != 8 {
                error!("Cashless device incorrect setup length {}", len);
                return None;
            }
        } else {
            error!("Cashless device failed to reply with setup data");
            return None;
        }

        //Parse the setup data from buffer
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

        //L3 features (only if we are a l3 reader)
        let supports_ftl;
        let monetary_format_32_bit;
        let supports_multicurrency;
        let supports_negative_vend;
        let supports_data_entry;
        let supports_always_idle;

        //Newly added L3 features in 2019 spec
        let supports_remote_vend;
        let supports_basket;
        let supports_coupon;
        let supports_ask_begin_session;
        let supports_enhanced_item_number_information;

        //Min max price data
        bus.send_data_and_confirm_ack(&VMC_MAX_MIN_PRICE_DATA).await;

        //Expansion request
        bus.send_data(&VMC_EXPANSION_REQUEST_ID_DATA).await; //as above
        if let Ok(MDBResponse::Data(len)) = bus.receive_response(&mut buf).await {
            if matches!(feature_level, CashlessDeviceFeatureLevel::Level3) {
                if len != 34 {
                    error!(
                        "L3 cashless device replied with wrong length expansion data ( {} )",
                        len
                    );
                    return None;
                }
            } else if len != 30 {
                //30 bytes if level 1-2
                error!(
                    "Non L3 cashless device replied with wrong length expansion data ( {} )",
                    len
                );
                return None;
            }
        } else {
            error!("Cashless device failed to reply with expansion request data");
            return None;
        };

        match feature_level {
            CashlessDeviceFeatureLevel::Level3 => {
                //Original level 3 features
                supports_ftl = buf[33] & 0x01 != 0;
                monetary_format_32_bit = buf[33] & 0x02 != 0;
                supports_multicurrency = buf[33] & 0x04 != 0;
                supports_negative_vend = buf[33] & 0x08 != 0;
                supports_data_entry = buf[33] & 0x10 != 0;
                supports_always_idle = buf[33] & 0x20 != 0;

                //Newly added L3 features in 2019 spec
                supports_remote_vend = buf[33] & 0x40 != 0;
                supports_basket = buf[33] & 0x80 != 0;
                supports_coupon = buf[32] & 0x01 != 0;
                supports_ask_begin_session = buf[32] & 0x02 != 0;
                supports_enhanced_item_number_information = buf[32] & 0x04 != 0;
            }
            _ => {
                //L1-2 readers wont support any of these
                supports_ftl = false;
                monetary_format_32_bit = false;
                supports_multicurrency = false;
                supports_negative_vend = false;
                supports_data_entry = false;
                supports_always_idle = false;

                //Newly added L3 features in 2019 spec
                supports_remote_vend = false;
                supports_basket = false;
                supports_coupon = false;
                supports_ask_begin_session = false;
                supports_enhanced_item_number_information = false;
            }
        }

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
            manufacturer_code: {
                match from_utf8(&buf[1..4]) {
                    Ok(a) => str4::from(a),
                    Err(_) => {
                        error!("Non-ascii text in mfr code");
                        str4::from("")
                    }
                }
            },
            serial_number: {
                match from_utf8(&buf[4..16]) {
                    Ok(a) => str16::from(a),
                    Err(_) => {
                        error!("Non-ascii text in mfr code");
                        str16::from("")
                    }
                }
            },
            model_number: {
                match from_utf8(&buf[16..28]) {
                    Ok(a) => str16::from(a),
                    Err(_) => {
                        error!("Non-ascii text in mfr code");
                        str16::from("")
                    }
                }
            },
            software_version: {
                match from_utf8(&buf[28..30]) {
                    Ok(a) => str4::from(a),
                    Err(_) => {
                        error!("Non-ascii text in mfr code");
                        str4::from("")
                    }
                }
            },
            //L3 features
            supports_ftl,
            monetary_format_32_bit,
            supports_multicurrency,
            supports_negative_vend,
            supports_data_entry,
            supports_always_idle,

            //Newly added L3 features in 2019 spec
            supports_remote_vend,
            supports_basket,
            supports_coupon,
            supports_ask_begin_session,
            supports_enhanced_item_number_information,
        };

        //Enable our desired optional features
        match bus.send_data_and_confirm_ack(&FEATURE_FLAG_DATA).await {
            true => debug!("Option feature enable command ACKd"),
            false => error!("Option feature enable command NAK"),
        }

        //Device not enabled by default, you'll need to enable it
        Some(c)
    }

    pub async fn record_cash_transaction<T: Read + Write>(
        &self,
        bus: &mut Mdb<T>,
        unscaled_amount: u16,
        address: [u8; 2],
    ) -> bool {
        let amount = unscaled_amount.to_le_bytes();
        bus.send_data_and_confirm_ack(&[
            VEND_PREFIX,
            VEND_CASH_SALE,
            amount[1],
            amount[0],
            address[0],
            address[1],
        ])
        .await
    }

    pub async fn start_transaction<T: Read + Write>(
        &self,
        bus: &mut Mdb<T>,
        unscaled_amount: u16,
        address: [u8; 2],
    ) -> bool {
        let amount = unscaled_amount.to_le_bytes();
        bus.send_data_and_confirm_ack(&[
            VEND_PREFIX,
            VEND_REQUEST,
            amount[1],
            amount[0],
            address[0],
            address[1],
        ])
        .await
    }

    pub async fn cancel_transaction<T: Read + Write>(&self, bus: &mut Mdb<T>) -> bool {
        bus.send_data_and_confirm_ack(&[VEND_PREFIX, VEND_CANCEL])
            .await
    }

    pub async fn vend_success<T: Read + Write>(&self, bus: &mut Mdb<T>, address: [u8; 2]) -> bool {
        bus.send_data_and_confirm_ack(&[VEND_PREFIX, VEND_SUCCESS, address[0], address[1]])
            .await
    }

    pub async fn vend_failed<T: Read + Write>(&self, bus: &mut Mdb<T>) -> bool {
        bus.send_data_and_confirm_ack(&[VEND_PREFIX, VEND_FAILURE])
            .await
    }

    pub async fn end_session<T: Read + Write>(&self, bus: &mut Mdb<T>) -> bool {
        bus.send_data_and_confirm_ack(&[VEND_PREFIX, VEND_SESSION_COMPLETE])
            .await
    }

    pub async fn set_device_enabled<T: Read + Write>(
        &self,
        bus: &mut Mdb<T>,
        enable: bool,
    ) -> bool {
        let cmd = if enable {
            VEND_READER_ENABLE
        } else {
            VEND_READER_DISABLE
        };
        bus.send_data_and_confirm_ack(&[VEND_READER_PREFIX, cmd])
            .await
    }

    pub async fn poll<T: Read + Write>(
        &self,
        bus: &mut Mdb<T>,
    ) -> [Option<PollEvent>; 36] {
        let mut events: [Option<PollEvent>; 36] = [None; 36];
        let mut buf: [u8; 64] = [0x00; 64];
        bus.send_data(&[POLL_CMD]).await;
        match bus.receive_response(&mut buf).await {
            Ok(response) => {
                match response {
                    MDBResponse::Data(len) => {
                        let mut event_count: usize = 0;
                        let mut index: usize = 0;
                        while index < len {
                            //Get the length of the first poll event in the buffer
                            match self.poll_response_length(buf[index]) {
                                Ok(event_len) => {
                                    debug!("Parsing poll event - size {}", event_len);
                                    //Create the event
                                    match PollEvent::try_from(&buf[index..index + event_len]) {
                                        Ok(event) => {
                                            debug!("Parsed a poll event: {=[u8]:#04x}", buf[index..index + event_len]);
                                            events[event_count] = Some(event);
                                            event_count += 1;
                                        }
                                        Err(_) => {
                                            error!(
                                                "Invalid poll event data: {=[u8]:#04x}",
                                                buf[index..index + event_len]
                                            );
                                        }
                                    }
                                    index += event_len;
                                }
                                Err(_) => {
                                    //If this byte is invalid, we cannot parse anything further.
                                    error!(
                                        "Invalid poll byte - abandoning further message parsing"
                                    );
                                    return events;
                                }
                            }
                        }
                    }
                    MDBResponse::StatusMsg(x) => {
                        //If we got an ACK, that means there aren't any events.
                        match x {
                            MDBStatus::NAK => {
                                error!("Cashless device poll NAK")
                            }
                            _ => {}
                        }
                    }
                }
            }
            Err(_) => {
                error!("Cashless poll generated MDB error");
            }
        };
        events
    }
}
