#![no_std]
pub mod coin_acceptor;
pub mod cashless_device;

use defmt::*;
use embedded_io_async::{Read, Write};
use embassy_time::{Duration, Timer};


pub enum MDBStatus {
    ACK,
    NAK,
    RET,
}

pub enum MDBError {
    Timeout,
    WrongChecksum,
    BufferOverrun,
    MalformedMessage,
    UartError,
}
pub enum MDBResponse<T, U> {
    Data(T),
    StatusMsg(U),
}

pub struct Mdb<T: Write + Read> {
    uart: T,
}

impl<T: Read + Write> Mdb<T> {
    pub fn new(uart : T) -> Self {
        Self { uart }
    }

    pub async fn send_status_message(&mut self, status: MDBStatus) {
        //Status messages do not have a checksum, nor the 'address' bit set
        let byte = match status {
            MDBStatus::ACK => 0x00u8,
            MDBStatus::NAK => 0xFFu8,
            MDBStatus::RET => 0xAAu8,
        };
        let _ = self.uart.write(&[byte, 0x00u8]).await;
    }

    pub async fn send_data(&mut self, msg: &[u8]) {
        let mut checksum: u8 = 0x00;
        for (i, byte) in msg.iter().enumerate() {
            //First byte is an address byte, 9th bit high
            let ninth_bit = if i == 0 { 0x01u8 } else { 0x00u8 };
            let _ = self.uart.write(&[ninth_bit, *byte]).await;
            //Update checksum calculation
            checksum = checksum.wrapping_add(*byte); //Note, 9th bit not included in checksum
        }
        let _ = self.uart.write(&[0x00, checksum]).await;
    }

    pub async fn send_data_and_confirm_ack(&mut self, msg: &[u8]) -> bool {
        let _ = self.send_data(msg).await;
        //We supply an empty buffer as we don't want any bytes received, only a status.
        match self.receive_response(&mut []).await {
            Ok(MDBResponse::StatusMsg(MDBStatus::ACK)) => {
                true
            },
            _ => {
                false
            },
        }
    }

    pub async fn receive_response(
        &mut self,
        buf: &mut [u8],
    ) -> Result<MDBResponse<usize, MDBStatus>, MDBError> {
        //We need a scratch buffer twice the maximum message length, because
        //2 bytes are returned by the 9 bit uart, with the first byte holding the ninth bit val.
        let mut scratch_buf: [u8; 72] = [0x00; 72];
        let mut calculated_checksum: u8 = 0x00;
        let mut bytes_out: usize = 0;

        match self.uart.read(&mut scratch_buf).await {
            Ok(count) => {
                match count {
                    0 => {
                        return Err(MDBError::Timeout);
                    }
                    2 => {
                        //This should be an ACK or NAK.
                        match scratch_buf[1] {
                            0x00 => {
                                return Ok(MDBResponse::StatusMsg(MDBStatus::ACK));
                            }
                            0xFF => {
                                return Ok(MDBResponse::StatusMsg(MDBStatus::NAK));
                            }
                            _ => {
                                error!(
                                    "Invalid 1 byte message - not NAK/ACK - was {=u8:#x}",
                                    scratch_buf[1]
                                );
                                return Err(MDBError::MalformedMessage);
                            }
                        }
                    }
                    _ => {
                        //Multibyte message.
                        //Check 9th bit on last byte - if it isn't 1, there's a problem!
                        if scratch_buf[count - 2] == 0x01 {
                            debug!("Full message received (last bit high)");
                            //Message complete
                            for (i, byte) in scratch_buf[0..count].iter().enumerate() {
                                //Only 'even' bytes are the 8 bits we're interested in - 9th bit doesnt count here
                                //Also, don't add the checksum to itself
                                if i % 2 != 0 && i != count - 1 {
                                    calculated_checksum = calculated_checksum.wrapping_add(*byte);
                                    if buf.len() <= i / 2 {
                                        error!("Buffer overrun - length insufficient");
                                        return Err(MDBError::BufferOverrun);
                                    }
                                    buf[i / 2] = *byte;
                                    bytes_out += 1;
                                }
                            }
                            if scratch_buf[count - 1] == calculated_checksum {
                                debug!("Message checksum correct - received {} bytes", bytes_out);
                                //Send ACK
                                self.send_status_message(MDBStatus::ACK).await;
                                return Ok(MDBResponse::Data(bytes_out));
                            } else {
                                error!(
                                    "Message checksum invalid - got {=u8:#x}, expected {=u8:#x}",
                                    scratch_buf[count - 1],
                                    calculated_checksum
                                );
                                return Err(MDBError::WrongChecksum);
                            }
                        } else {
                            error!("Malformed/incomplete message (last bit not high)");
                            return Err(MDBError::MalformedMessage);
                        }
                    }
                }
            }
            Err(_) => {
                return Err(MDBError::UartError);
            }
        };
    }
}
