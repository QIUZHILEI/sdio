#![no_std]
#![feature(error_generic_member_access)]
#![feature(const_option)]
mod cmd;
pub mod err;
mod ops;
mod reg;
mod sd_reg;

use cmd::*;

use log::{debug, info, trace};
use ops::*;
use reg::*;
use sd_reg::*;
use tom_device::{
    read_reg, write_reg, BlockDevice, Device, DeviceError, DeviceStatus, DeviceType, SectorSize,
};
use tom_timer::{Delay, Ticker};
pub struct DwMmcHost {
    sdio_base: usize,
    rca: Rca,
    ocr: Ocr,
    cic: Cic,
    cid: Cid,
    csd: Csd,
    hard_config: HardConf,
    mmc_opt: MmcOperate,
    delay: Delay,
}

impl DwMmcHost {
    pub const fn new(sdio_base: usize, ticker: &'static dyn Ticker) -> Self {
        let mmc = MmcOperate::new(sdio_base, ticker);
        Self {
            sdio_base,
            rca: Rca::new(),
            ocr: Ocr::new(),
            cic: Cic::new(),
            cid: Cid::new(),
            csd: Csd::new(),
            hard_config: HardConf(0),
            mmc_opt: mmc,
            delay: Delay::new(ticker),
        }
    }
}
impl Device for DwMmcHost {
    fn init(&mut self) -> Result<(), DeviceError> {
        info!("init sdio...");
        let hconf = HardConfig::from_bits(read_reg::<u32>(self.sdio_base, REG_HCON)).unwrap();
        debug!("{hconf:?}");
        self.hard_config = HardConf::from(hconf.bits());
        // Reset Control Register
        let reset_mask = ControlMask::controller_reset.bits()
            | ControlMask::fifo_reset.bits()
            | ControlMask::dma_reset.bits();
        write_reg::<u32>(self.sdio_base, REG_CTRL, reset_mask);
        self.mmc_opt.wait_reset(reset_mask)?;
        // enable power
        write_reg::<u32>(self.sdio_base, REG_PWREN, 1);
        self.mmc_opt.reset_clock(1, 62)?;
        write_reg::<u32>(self.sdio_base, REG_TMOUT, 0xFFFFFFFF);
        // setup interrupt mask
        write_reg::<u32>(self.sdio_base, REG_RINTSTS, InterruptMask::all().bits());
        write_reg::<u32>(self.sdio_base, REG_INTMASK, 0);
        write_reg::<u32>(self.sdio_base, REG_CTYPE, 1);
        write_reg::<u32>(self.sdio_base, REG_IDINTEN, 0);
        write_reg::<u32>(self.sdio_base, REG_BMOD, 1);

        // // enumerate card stack
        self.mmc_opt.send_cmd(idle())?;
        self.delay.spin_millis(10);
        self.cic = self.mmc_opt.check_version()?;
        self.ocr = self.mmc_opt.check_v18_sdhc()?;
        self.cid = self.mmc_opt.check_cid()?;
        self.rca = self.mmc_opt.check_rca()?;
        self.csd = self.mmc_opt.check_csd(self.rca)?;
        self.mmc_opt.sel_card(self.rca)?;
        self.mmc_opt.function_switch(16777201)?;
        self.mmc_opt.set_bus(self.rca)?;
        self.mmc_opt.reset_clock(1, 1)?;
        write_reg::<u32>(
            self.sdio_base,
            REG_IDINTEN,
            (DmaIntEn::ri | DmaIntEn::ti).bits(),
        );
        info!("sdio init success!");
        Ok(())
    }

    fn close(&mut self) -> Result<(), DeviceError> {
        Ok(())
    }

    fn status(&self) -> DeviceStatus {
        DeviceStatus::Initializing
    }

    fn reinit(&mut self) -> Result<(), tom_device::DeviceError> {
        self.init()
    }

    fn device_type(&self) -> DeviceType {
        DeviceType::Block
    }

    fn error_handle(&self) -> DeviceStatus {
        DeviceStatus::Error
    }
}

impl BlockDevice for DwMmcHost {
    fn physical_block_size(&self) -> usize {
        512
    }

    fn sector_size(&self) -> SectorSize {
        SectorSize::Lb512
    }

    fn read_block(&mut self, lba: usize, buf: &mut [u8]) -> Result<(), DeviceError> {
        trace!("read block, address: {},", lba);
        let cmd = read_single_block(lba as u32);
        match self.mmc_opt.send_cmd(cmd) {
            Ok(resp) => {
                let status = resp.card_status();
                debug!("{status:?}");
                let blk_sz = self.physical_block_size() as u32;
                let blk = buf.len() as u32 / blk_sz;
                match self.mmc_opt.read_data(buf, blk, blk_sz) {
                    Ok(_) => Ok(()),
                    Err(err) => {
                        debug!("{err:?}");
                        self.mmc_opt.stop_transmission_ops()?;
                        Err(DeviceError::IoError)
                    }
                }
            }
            Err(err) => {
                debug!("{err:?}");
                self.mmc_opt.stop_transmission_ops()?;
                Err(DeviceError::IoError)
            }
        }
    }

    fn write_block(&self, lba: usize, data: &[u8]) -> Result<(), DeviceError> {
        let cmd = write_single_block(lba as u32);
        match self.mmc_opt.send_cmd(cmd) {
            Ok(resp) => {
                let status = resp.card_status();
                debug!("{status:?}");
                let blk_sz = self.physical_block_size() as u32;
                let blk = data.len() as u32 / blk_sz;
                match self.mmc_opt.write_data(data, blk, blk_sz) {
                    Ok(_) => Ok(()),
                    Err(err) => {
                        debug!("{err:?}");
                        self.mmc_opt.stop_transmission_ops()?;
                        Err(DeviceError::IoError)
                    }
                }
            }
            Err(err) => {
                debug!("{err:?}");
                self.mmc_opt.stop_transmission_ops()?;
                Err(DeviceError::IoError)
            }
        }
    }
}
