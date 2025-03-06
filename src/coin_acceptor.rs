use crate::MDBResponse;
use crate::MDBStatus;
use crate::Mdb;

use embedded_io_async::{Read, Write};
use embassy_time::Timer;

use core::str::from_utf8;

use defmt::*;

use fixedstr::{str4, str16};
//All coin acceptors should support these commands
const RESET_CMD: u8 = 0x08;
const SETUP_CMD: u8 = 0x09;
const TUBE_STATUS_CMD: u8 = 0x0A;
const POLL_CMD: u8 = 0x0B;
const COIN_TYPE_CMD: u8 = 0x0C;
const DISPENSE_CMD: u8 = 0x0D;

//Level 3 'expansion' commands all start with 0x0F
const L3_CMD_PREFIX: u8 = 0x0F;

//These should only be sent to a coin acceptor that identifies as supporting L3
const L3_IDENT_CMD: u8 = 0x00;
const L3_FEATURE_ENABLE_CMD: u8 = 0x01;
const L3_PAYOUT_CMD: u8 = 0x02;
const L3_PAYOUT_STATUS_CMD: u8 = 0x03;
const L3_PAYOUT_VALUE_POLL_CMD: u8 = 0x04;
const L3_DIAG_CMD: u8 = 0x05;

pub enum L3OptionalFeature {
    AltPayout = 0x01,
    ExtDiag = 0x02,
    ControlledFillAndPayout = 0x04,
    Ftl = 0x08,
}

pub struct CoinAcceptor {
    pub feature_level: CoinAcceptorLevel,
    pub country_code: [u8; 2],
    pub scaling_factor: u8,
    pub decimal_places: u8,
    pub coin_types: [Option<CoinType>; 16],
    pub l3_features: Option<CoinAcceptorL3Features>,
}

pub struct CoinAcceptorL3Features {
    pub manufacturer_code: str4,
    pub serial_number: str16,
    pub model: str16,
    pub software_ver: str4,

    pub alt_payout_cmd_supported: bool,
    pub ext_diag_cmd_supported: bool,
    pub controlled_fill_payout_cmd_supported: bool,
    pub ftl_cmd_supported: bool,
}

#[derive(Copy, Clone, Format)]
pub struct CoinType {
    pub unscaled_value: u16,
    pub routeable_to_tube: bool,
    pub tube_full: bool,
    pub num_coins: u8,
}

#[derive(Copy, Clone)]
pub struct CoinInsertedEvent {
    pub coin_type: u8,        //What number coin it is
    pub unscaled_value: u16,  //Unscaled value
    pub routing: CoinRouting, //where it was routed to
    pub coins_remaining: u8,  //what the coin acceptor thinks the tube count now is
}

#[derive(Copy, Clone)]
pub struct ManualDispenseEvent {
    pub coin_type: u8,       //type of the coin
    pub unscaled_value: u16, //unscaled value
    pub number: u8,          //Number of coins dispensed
    pub coins_remaining: u8, //Remaining coins
}

//A poll event might be one of the following:
#[derive(Copy, Clone)]
pub enum PollEvent {
    //Slugs inserted since last poll
    SlugCount(u8),
    Status(u8),
    Coin(CoinInsertedEvent),
    ManualDispense(ManualDispenseEvent),
}

#[derive(Format, Copy, Clone)]
pub enum CoinRouting {
    CashBox,
    Tube,
    Reject,
    Unknown,
}

#[derive(Format)]
pub enum CoinAcceptorLevel {
    Level2,
    Level3,
}

impl CoinAcceptor {
    pub async fn init<T: Read + Write> (bus: &mut Mdb<T>) -> Option<Self> {
        //Start with a reset
        bus.send_data_and_confirm_ack(&[RESET_CMD]).await;

        //Give it 100mS to get over its' reset
        Timer::after_millis(100).await;

        //Then a poll command - handled manually as object not yet initialised
        bus.send_data(&[POLL_CMD]).await;
        let mut buf = [0x00; 48];
        if let Ok(MDBResponse::Data(size)) = bus.receive_response(&mut buf).await {
            if size == 1 && matches!(buf[0], 0x0B) {
                debug!("Initial poll succesful - just reset");
            }
            else {
                error!("Unexpected poll reply {=u8:#x}",buf[0])
            }
        }

        //Now send a setup command
        bus.send_data(&[SETUP_CMD]).await;

        if let Ok(MDBResponse::Data(size)) = bus.receive_response(&mut buf).await {
            if size != 23 {
                defmt::debug!("Error - coin acceptor init received incorrect byte count");
                return None;
            }
            let mut coinacceptor = CoinAcceptor {
                feature_level: match buf[0] {
                    0x02 => CoinAcceptorLevel::Level2,
                    0x03 => CoinAcceptorLevel::Level3,
                    _ => {
                        defmt::debug!("Coin acceptor reported unknown feature level - assuming L2");
                        CoinAcceptorLevel::Level2
                    }
                },
                country_code: buf[1..3].try_into().unwrap(),
                scaling_factor: buf[3],
                decimal_places: buf[4],
                l3_features: None,
                coin_types: {
                    //Parse the coin type data
                    let mut types: [Option<CoinType>; 16] = [None; 16];
                    let mut type_count: usize = 0;
                    for (index, byte) in buf[7..23].into_iter().enumerate() {
                        if *byte != 0x00 {
                            types[type_count] = Some(CoinType {
                                unscaled_value: *byte as u16 * buf[3] as u16,
                                tube_full: false,
                                num_coins: 0,
                                routeable_to_tube: ((buf[5] as u16) << 8 | buf[6] as u16)
                                    & (0x01 << index)
                                    != 0,
                            });
                            type_count += 1;
                        }
                    }
                    types
                },
            };

            defmt::debug!("Initial coin acceptor discovery complete");
            //If this is a level 3 coin acceptor, we need to discover its' level 3 features here
            if matches!(coinacceptor.feature_level, CoinAcceptorLevel::Level3) {
                defmt::debug!("Probing L3 features");
                //interrogate Level 3 dispensers to discover device details and features supported
                bus.send_data(&[L3_CMD_PREFIX, L3_IDENT_CMD]).await;
                if let Ok( MDBResponse::Data(size)) = bus.receive_response(&mut buf).await {
                    if size != 33 {
                        defmt::debug!(
                            "Coin acceptor L3 identify command received wrong length reply"
                        );
                    } else {
                        let l3 = CoinAcceptorL3Features {
                            manufacturer_code: {
                                match from_utf8(&buf[0..3]) {
                                    Ok(a) => str4::from(a),
                                    Err(_) => {
                                        error!("Non-ascii text in mfr code");
                                        str4::from("")
                                    }
                                }
                            },
                            serial_number: {
                                match from_utf8(&buf[3..15]) {
                                    Ok(a) => str16::from(a),
                                    Err(_) => {
                                        error!("Non-ascii text in serial number");
                                        str16::from("")
                                    }                                
                                }
                            },
                            model: {
                                match from_utf8(&buf[15..27]) {
                                    Ok(a) => str16::from(a),
                                    Err(_) => {
                                        error!("Non-ascii text in model number");
                                        str16::from("")
                                    }                                
                                }
                            },
                            software_ver: {
                                match from_utf8(&buf[27..29]) {
                                    Ok(a) => str4::from(a),
                                    Err(_) => {
                                        error!("Non-ascii text in s/w ver");
                                        str4::from("")
                                    }                                
                                }
                            },
                            alt_payout_cmd_supported: {
                                buf[32] & L3OptionalFeature::AltPayout as u8
                                    == L3OptionalFeature::AltPayout as u8
                            },
                            ext_diag_cmd_supported: {
                                buf[32] & L3OptionalFeature::ExtDiag as u8
                                    == L3OptionalFeature::ExtDiag as u8
                            },
                            controlled_fill_payout_cmd_supported: {
                                buf[32] & L3OptionalFeature::ControlledFillAndPayout as u8
                                    == L3OptionalFeature::ControlledFillAndPayout as u8
                            },
                            ftl_cmd_supported: {
                                buf[32] & L3OptionalFeature::Ftl as u8
                                    == L3OptionalFeature::Ftl as u8
                            },
                        };

                        //Enable the features we want to use
                        let mut features_to_enable: u8 = 0x00;
                        if l3.alt_payout_cmd_supported {
                            features_to_enable |= L3OptionalFeature::AltPayout as u8;
                        }
                        if l3.ext_diag_cmd_supported {
                            features_to_enable |= L3OptionalFeature::ExtDiag as u8;
                        }
                        if coinacceptor.l3_enable_features(bus, features_to_enable).await.is_ok() {
                            debug!("L3 features enabled OK");
                        } else {
                            error!("L3 features failed to enable");
                        }

                        //Store the L3 features struct into the coin acceptor
                        coinacceptor.l3_features = Some(l3);
                    }
                }
            }

            defmt::debug!("Updating coin counts");
            //Now probe the coin counts and update the above statuses
            let _ =  coinacceptor.update_coin_counts(bus).await;

            return Some(coinacceptor);
        }
        return None;
    }
    
    pub async fn l3_enable_features<T: Read + Write>(
        &mut self,
        bus: &mut Mdb<T>,
        feature_mask: u8,
    ) -> Result<(),()> {
        if !matches!(self.feature_level, CoinAcceptorLevel::Level3) {
            error!("Tried to enable L3 features on non L3 coin acceptor");
            Err(())
        } else {
            if bus.send_data_and_confirm_ack (
                &[
                    L3_CMD_PREFIX,
                    L3_FEATURE_ENABLE_CMD,
                    0x00,
                    0x00,
                    0x00,
                    feature_mask
                ]).await {
                    debug!("Level 3 features enabled (flags {})", feature_mask);
                    Ok(())
                }
            else {
                error!("Failed to receive ACK to l3 feature enable cmd");
                Err(())
            }
        }        
    }

    async fn update_coin_counts<T: Read + Write>(&mut self, bus: &mut Mdb<T>) -> Result<(),()> {
        bus.send_data(&[TUBE_STATUS_CMD]).await;

        let mut buf: [u8; 18] = [0x00; 18];
        if let Ok(MDBResponse::Data(count)) = bus.receive_response(&mut buf).await {
            if count != 18 {
                error!("Incorrect reply length -{}", count);
                return Err(())
            }
            let tube_full_status: u16 = (buf[0] as u16) << 8 | (buf[1] as u16) << 8;

            for i in 0..16 {
                if let Some(mut cointype) = self.coin_types[i].take() {
                    cointype.num_coins = buf[i + 2];
                    cointype.tube_full = tube_full_status & 0x01 << i != 0x00;
                    self.coin_types[i] = Some(cointype);
                }
            }
            debug!("Coin counts updated");
            return Ok(());
        }
        else {
            error!("Incorrect response to coin count update request");
            return Err(());
        }
    }

    pub async fn enable_coins<T: Read + Write>(
        &mut self,
        bus: &mut Mdb<T>,
        coin_mask: u16,
    ) -> Result<(), ()> {
        //Which coins you want to enable - NB We enable manual dispense for all coins automatically.
        if bus.send_data_and_confirm_ack(&[
            COIN_TYPE_CMD,
            (coin_mask & 0xFF) as u8,
            ((coin_mask >> 8) & 0xFF) as u8,
            0xFF,
            0xFF,
        ]).await {
            debug!("Coins enabled OK");
            Ok(())
        }
        else {
            error!("Coins not enabled");
            Err(())
        }
    }

    pub async fn payout<T: embedded_io_async::Write + embedded_io_async::Read>(
        &mut self,
        bus: &mut Mdb<T>,
        credit: u16,
    ) -> u16 {
        let use_l3_payout = if let Some(l3features) = &self.l3_features {
            l3features.alt_payout_cmd_supported
        } else {
            false
        };

        let amount_paid =  if use_l3_payout {
            self.payout_level3(bus, credit).await
        } else {
            self.payout_level2(bus, credit).await
        };

        if amount_paid == credit {
            defmt::info!("Payout complete");
        } else {
            defmt::info!(
                "Error - incomplete payout.  Requested {}, paid {}",
                credit,
                amount_paid
            );
        };
        //Update the coin coints
        let _ = self.update_coin_counts(bus).await;

        amount_paid
    }

    pub async fn payout_level2<T: embedded_io_async::Write + embedded_io_async::Read>(
        &mut self,
        bus: &mut Mdb<T>,
        credit: u16,
    ) -> u16 {
        defmt::debug!("Starting Level 2 Payout");
        let mut amount_paid: u16 = 0;
        //Reverse order, so starting with the highest valued coins first
        for (i, c) in self.coin_types.iter().enumerate().rev() {
            if let Some(coin) = c {
                let mut num_to_pay = ((credit - amount_paid) / coin.unscaled_value) as u8;

                //Cannot pay out more coins than we have in the tube
                if num_to_pay as u8 > coin.num_coins {
                    num_to_pay = coin.num_coins;
                }

                while num_to_pay > 0 {
                    //Each command can only pay out 16 coins max, so if we want to
                    //dispense more than 16, we have to send multiple commands
                    let num_to_dispense = if num_to_pay > 16 { 16 } else { num_to_pay };
                    //send the command
                    let b: u8 = i as u8 | num_to_pay << 4;
                    defmt::debug!(
                        "Aiming to dispense {=u8} coins of type {=usize}, value {=u16}",
                        num_to_pay,
                        i,
                        coin.unscaled_value
                    );
                    if bus.send_data_and_confirm_ack(&[DISPENSE_CMD, b]).await {
                        defmt::debug!("Payout cmd acked - payout in progress");
                        amount_paid += coin.unscaled_value * num_to_pay as u16;
                        num_to_pay -= num_to_dispense;
                    } else {
                        defmt::debug!("Payout cmd not acked")
                    }
                }
            }
            if amount_paid == credit {
                break;
            }
        }
        amount_paid
    }

    pub async fn payout_level3<T: embedded_io_async::Write + embedded_io_async::Read>(
        &mut self,
        bus: &mut Mdb<T>,
        credit: u16,
    ) -> u16 {
        defmt::debug!("Starting Level 3 Payout");
        let credit_scaled = credit / self.scaling_factor as u16;
        if credit_scaled > 255 {
            defmt::debug!("Payout value exceeds allowable limit");
            0
        } else {
            bus.send_data_and_confirm_ack(&[L3_CMD_PREFIX, L3_PAYOUT_CMD, credit_scaled as u8]).await;

            let mut buf: [u8; 16] = [0x00; 16];
            let mut complete: bool = false;

            while !complete {
                bus.send_data(&[L3_CMD_PREFIX, L3_PAYOUT_VALUE_POLL_CMD]).await;
                match bus.receive_response(&mut buf).await {
                    Ok(MDBResponse::Data(_count)) => {
                        //This is the amount of credit paid out so far, not that interested for now
                    }
                    Ok(MDBResponse::StatusMsg(x)) => match x {
                        MDBStatus::ACK => {
                            complete = true;
                        }
                        _ => {}
                    },
                    _=>{}
                }
            }
            let mut amount_paid: u16 = 0;

            bus.send_data(&[L3_CMD_PREFIX, L3_PAYOUT_STATUS_CMD]).await;
            match bus.receive_response(&mut buf).await {
                Ok(MDBResponse::Data(count)) => {
                    for (i, byte) in buf[0..count].iter().enumerate() {
                        self.coin_types[i].and_then(|ct| {
                            amount_paid += ct.unscaled_value * *byte as u16;
                            Some(ct)
                        });
                    }
                }
                _ => {}
            }

            amount_paid
        }
    }

    pub async fn poll<T:Write + Read>(
        &mut self,
        bus: &mut Mdb<T>,
    ) -> Result<[Option<PollEvent>; 16], ()> {
        //You might get up to 16 poll events and you should process them in order..
        let mut poll_results: [Option<PollEvent>; 16] = [None; 16];
        let mut result_count: usize = 0;

        //Send poll command
        bus.send_data(&[POLL_CMD]).await;

        //Read poll response - max 16 bytes
        let mut buf: [u8; 16] = [0x00; 16];
    
        //Parse response
        if let Ok(response) = bus.receive_response(&mut buf).await {
            match response {
                MDBResponse::StatusMsg(status) => { 
                    if matches!(status, MDBStatus::ACK) {
                        //nothing to report;
                        return Ok(poll_results);
                    }
                }
                MDBResponse::Data(count) => {
                    debug!("Parsing byte count {}, {=[u8]:#04x}", count, buf[0..count]);
                    //small state machine to handle 2 byte nature of potential messages.
                    enum ParseState {
                        ManualDispense(u8),
                        CoinDeposited(u8),
                        NoState,
                    }
                    let mut state: ParseState = ParseState::NoState;

                    for byte in &buf[0..count] {
                        match state {
                            ParseState::NoState => {
                                if byte & 0x80 == 0x80 {
                                    //Enter manual dispense parse, and wait for byte 2 to arrive
                                    state = ParseState::ManualDispense(*byte);
                                } else if byte & 0x40 == 0x40 {
                                    //Enter coin deposited state, and wait for byte 2 to arrive
                                    state = ParseState::CoinDeposited(*byte);
                                } else if byte & 0x20 == 0x20 {
                                    //FYI: Slugs are 'items' not recognised as valid coins
                                    //US English term apparently - eg a washer to try to fool the acceptor.
                                    poll_results[result_count] =
                                        Some(PollEvent::SlugCount(byte & 0x1F));
                                    result_count += 1;
                                } else {
                                    //It's a status - transcribe the byte across
                                    poll_results[result_count] = Some(PollEvent::Status(*byte));                                    
                                };
                            }
                            ParseState::CoinDeposited(b) => {
                                ////Someone has deposited a coin
                                poll_results[result_count] = Some(PollEvent::Coin(CoinInsertedEvent {
                                    coin_type: b & 0x0F,
                                    unscaled_value: {
                                        if let Some(ct) = self.coin_types[(b & 0x0F) as usize] {
                                            ct.unscaled_value
                                        } else {
                                            error!("Non existent coin deposited!");
                                            0
                                        }
                                    },
                                    // * self.scaling_factor as u16,
                                    routing: {
                                        match b & 0x30 {
                                            0x00 => CoinRouting::CashBox,
                                            0x10 => CoinRouting::Tube,
                                            0x30 => CoinRouting::Reject,
                                            _ => {
                                                // shouldn't happen...
                                                error!("Unexpected coin routing direction - {}", b&0x30);
                                                CoinRouting::Unknown
                                            }
                                        }
                                    },
                                    coins_remaining: *byte,
                                }));
                                result_count += 1;

                                //Reset the state machine
                                state = ParseState::NoState;
                            }
                            ParseState::ManualDispense(b) => {
                                poll_results[result_count] =
                                    Some(PollEvent::ManualDispense(ManualDispenseEvent {
                                        coin_type: b & 0x0F,
                                        unscaled_value: {
                                            if let Some(ct) = self.coin_types[(b & 0x0F) as usize] {
                                                ct.unscaled_value
                                            } else {
                                                error!("Non existent coin manually dispensed!");
                                                0
                                            }
                                        },
                                        number: (b >> 4) & 0x07,
                                        coins_remaining: *byte,
                                    }));
                                result_count += 1;
                                //Reset the state machine
                                state = ParseState::NoState;
                            }
                        }
                   }   
                }
            }
            Ok(poll_results)
        }
        else {
            Err(())
        }
    }

    pub async fn l3_diagnostic_status<T: embedded_io_async::Write + embedded_io_async::Read>(
        &mut self,
        bus: &mut Mdb<T>,
    ) -> [Option<[u8;2]>; 8] {

        let mut statuses: [Option<[u8;2]>; 8] = [None; 8];
        let mut num_statuses: usize = 0;

        if ! matches!(self.feature_level, CoinAcceptorLevel::Level3) {
            error!("Cannot get L3 diagnostic status on non L3 mech");
            return statuses;
        };

        bus.send_data(&[L3_CMD_PREFIX, L3_DIAG_CMD]).await;

        let mut buf: [u8; 16] = [0x00; 16];
        match bus.receive_response(&mut buf).await {
            Ok(MDBResponse::Data(len)) => {
                //Two byte statemachine for parsing
                pub enum State {
                    AwaitingFirstByte,
                    AwaitingSecondByte(u8), //u8 = firstbyte
                }
                let mut parser_state = State::AwaitingFirstByte;

                for byte in &buf[0..len] {
                    match parser_state {
                        State::AwaitingFirstByte => {
                            parser_state = State::AwaitingSecondByte(*byte);
                        }
                        State::AwaitingSecondByte(firstbyte) => {
                            //Store the status into the return array now both bytes have arrived
                            statuses[num_statuses] = Some([firstbyte, *byte]);
                            debug!("Recorded status of {=u8:#x} {=u8:#x}", firstbyte, *byte);
                            num_statuses += 1;
                            //Reset the parser ready for the first byte of the next error code pair
                            parser_state = State::AwaitingFirstByte;
                        }
                    }
                }
            },
            _=> {
                error!("Unexpected mdb response to L3 poll");
            }
        }
        statuses
    }
}