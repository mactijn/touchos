//! Registers, bits and default tables of the RT2800 family, as far as
//! the RT5390/RF5370 path uses them. From OpenBSD's `sys/dev/ic/rt2860reg.h`
//! (ISC, see the notice in lib.rs); the names keep OpenBSD's, without
//! the RT2860_/RT3070_/RT5390_ prefixes.

// ---- USB vendor requests (RT2870 and later) ----
/// DEVICE_MODE: wValue 1 = reset, 8 = start the firmware; IN with wIndex
/// 0x11 = read the autorun mode (rt2800usb; this driver doesn't ask, see
/// `Rt2800::init`).
pub const REQ_DEVICE_MODE: u8 = 1;
/// SINGLE_WRITE: one 16-bit value (wValue) to a register (wIndex).
pub const REQ_WRITE_2: u8 = 2;
/// MULTI_WRITE: the data stage to registers from wIndex on.
pub const REQ_MULTI_WRITE: u8 = 6;
/// MULTI_READ: registers from wIndex on into the data stage.
pub const REQ_MULTI_READ: u8 = 7;
/// EEPROM_READ: EEPROM bytes from wIndex on (not used with efuse).
pub const REQ_EEPROM_READ: u8 = 9;

pub const MODE_RESET: u16 = 1;
pub const MODE_FIRMWARE: u16 = 8;
pub const MODE_AUTORUN: u16 = 0x11;

// ---- Registers ----
pub const OPT_14: u16 = 0x0114;
pub const WPDMA_GLO_CFG: u16 = 0x0208;
pub const WMM_AIFSN_CFG: u16 = 0x0214;
pub const WMM_CWMIN_CFG: u16 = 0x0218;
pub const WMM_CWMAX_CFG: u16 = 0x021c;
pub const WMM_TXOP0_CFG: u16 = 0x0220;
pub const WMM_TXOP1_CFG: u16 = 0x0224;
pub const USB_DMA_CFG: u16 = 0x02a0;
pub const US_CYC_CNT: u16 = 0x02a4;
/// PBF_SYS_CTRL in Linux.
pub const SYS_CTRL: u16 = 0x0400;
pub const HOST_CMD: u16 = 0x0404;
pub const PBF_CFG: u16 = 0x0408;
pub const MAX_PCNT: u16 = 0x040c;
pub const BCN_OFFSET0: u16 = 0x042c;
pub const RF_CSR_CFG: u16 = 0x0500;
pub const EFUSE_CTRL: u16 = 0x0580;
pub const EFUSE_DATA3: u16 = 0x059c;
/// MAC_CSR0 in Linux: chip (high half) and revision (low half).
pub const ASIC_VER_ID: u16 = 0x1000;
pub const MAC_SYS_CTRL: u16 = 0x1004;
pub const MAC_ADDR_DW0: u16 = 0x1008;
pub const MAC_ADDR_DW1: u16 = 0x100c;
pub const MAC_BSSID_DW0: u16 = 0x1010;
pub const MAC_BSSID_DW1: u16 = 0x1014;
pub const BBP_CSR_CFG: u16 = 0x101c;
pub const LED_CFG: u16 = 0x102c;
pub const XIFS_TIME_CFG: u16 = 0x1100;
pub const BKOFF_SLOT_CFG: u16 = 0x1104;
pub const BCN_TIME_CFG: u16 = 0x1114;
pub const MAC_STATUS_REG: u16 = 0x1200;
pub const PWR_PIN_CFG: u16 = 0x1204;
pub const AUTO_WAKEUP_CFG: u16 = 0x1208;
pub const fn tx_pwr_cfg(ridx: u16) -> u16 {
    0x1314 + ridx * 4
}
pub const TX_PIN_CFG: u16 = 0x1328;
pub const TX_BAND_CFG: u16 = 0x132c;
pub const TX_SW_CFG0: u16 = 0x1330;
pub const TX_SW_CFG1: u16 = 0x1334;
pub const TX_SW_CFG2: u16 = 0x1338;
pub const TXOP_CTRL_CFG: u16 = 0x1340;
pub const TX_RTS_CFG: u16 = 0x1344;
pub const TX_TIMEOUT_CFG: u16 = 0x1348;
pub const TX_RTY_CFG: u16 = 0x134c;
pub const TX_LINK_CFG: u16 = 0x1350;
/// HT rate fallback on retries: MCS 0-7 (CFG0) and 8-15 (CFG1), 4 bits each.
pub const HT_FBK_CFG0: u16 = 0x1354;
pub const HT_FBK_CFG1: u16 = 0x1358;
/// Legacy rate fallback on retries: OFDM (CFG0) and CCK (CFG1).
pub const LG_FBK_CFG0: u16 = 0x135c;
pub const LG_FBK_CFG1: u16 = 0x1360;
pub const CCK_PROT_CFG: u16 = 0x1364;
pub const OFDM_PROT_CFG: u16 = 0x1368;
pub const MM20_PROT_CFG: u16 = 0x136c;
pub const MM40_PROT_CFG: u16 = 0x1370;
pub const GF20_PROT_CFG: u16 = 0x1374;
pub const GF40_PROT_CFG: u16 = 0x1378;
pub const EXP_ACK_TIME: u16 = 0x1380;
pub const RX_FILTR_CFG: u16 = 0x1400;
pub const AUTO_RSP_CFG: u16 = 0x1404;
pub const LEGACY_BASIC_RATE: u16 = 0x1408;
pub const HT_BASIC_RATE: u16 = 0x140c;
pub const TXOP_HLDR_ET: u16 = 0x1608;
/// TX statistics, clear on read: CNT0 = beacons (high half), frames that
/// failed (no ACK after all retries; low half); CNT1 = retransmissions
/// (high), frames that succeeded (low); CNT2 = underflows, zero-length.
pub const TX_STA_CNT0: u16 = 0x170c;
pub const WCID_ENTRY0: u16 = 0x1800;
pub const FW_BASE: u16 = 0x3000;
pub const WCID_ATTR0: u16 = 0x6800;
/// The pairwise key table: 32 bytes per WCID (the key, then TKIP's MIC
/// keys), `RT2860_PKEY`.
pub const PKEY0: u16 = 0x4000;
/// The IV/EIV table: 8 bytes per WCID, the CCMP header the chip sends
/// with when the TXWI doesn't bring its own (`RT2860_IVEIV`).
pub const IVEIV0: u16 = 0x6000;
pub const SKEY0: u16 = 0x6c00;
pub const SKEY_MODE_0_7: u16 = 0x7000;
pub const H2M_MAILBOX: u16 = 0x7010;
pub const H2M_MAILBOX_CID: u16 = 0x7014;
pub const H2M_MAILBOX_STATUS: u16 = 0x701c;
pub const H2M_INTSRC: u16 = 0x7024;
pub const H2M_BBPAGENT: u16 = 0x7028;

// ---- Bits ----
// WPDMA_GLO_CFG
pub const TX_WB_DDONE: u32 = 1 << 6;
pub const RX_DMA_BUSY: u32 = 1 << 3;
pub const RX_DMA_EN: u32 = 1 << 2;
pub const TX_DMA_BUSY: u32 = 1 << 1;
pub const TX_DMA_EN: u32 = 1 << 0;
// USB_DMA_CFG
pub const USB_TX_EN: u32 = 1 << 23;
pub const USB_RX_EN: u32 = 1 << 22;
pub const USB_RX_AGG_EN: u32 = 1 << 21;
/// RX aggregation limit, in KiB.
pub const fn usb_rx_agg_lmt(kib: u32) -> u32 {
    kib << 8
}
/// RX aggregation timeout, in units of 33 ns.
pub const fn usb_rx_agg_to(t: u32) -> u32 {
    t & 0xff
}
// SYS_CTRL
pub const PME_OEN: u32 = 1 << 13;
pub const MCU_READY: u32 = 1 << 7;
// RF_CSR_CFG
pub const RF_KICK: u32 = 1 << 17;
pub const RF_WRITE: u32 = 1 << 16;
// EFUSE_CTRL
pub const SEL_EFUSE: u32 = 1 << 31;
pub const EFSROM_KICK: u32 = 1 << 30;
pub const EFSROM_AIN_MASK: u32 = 0x03ff_0000;
pub const EFSROM_AIN_SHIFT: u32 = 16;
pub const EFSROM_MODE_MASK: u32 = 0x0000_00c0;
pub const EFUSE_AOUT_MASK: u32 = 0x0000_003f;
// MAC_SYS_CTRL
pub const MAC_RX_EN: u32 = 1 << 3;
/// Set only when the chip was brought up to transmit (`Rt2800::init`).
pub const MAC_TX_EN: u32 = 1 << 2;
pub const BBP_HRST: u32 = 1 << 1;
pub const MAC_SRST: u32 = 1 << 0;
// BBP_CSR_CFG
pub const BBP_CSR_KICK: u32 = 1 << 17;
pub const BBP_CSR_READ: u32 = 1 << 16;
// BCN_TIME_CFG
pub const BCN_TX_EN: u32 = 1 << 20;
pub const TBTT_TIMER_EN: u32 = 1 << 19;
pub const TSF_SYNC_MODE_STA: u32 = 1 << 17;
/// An access point: the TSF syncs with nobody (rt2860.c's
/// `rt2860_enable_tsf_sync` for HOSTAP).
pub const TSF_SYNC_MODE_HOSTAP: u32 = 3 << 17;
pub const TSF_TIMER_EN: u32 = 1 << 16;
/// The beacon interval field: in units of 1/16 TU.
pub const BCN_INTVAL_MASK: u32 = 0xffff;
// MAC_STATUS_REG
pub const RX_STATUS_BUSY: u32 = 1 << 1;
pub const TX_STATUS_BUSY: u32 = 1 << 0;
// TX_PIN_CFG
pub const TRSW_EN: u32 = 1 << 18;
pub const RFTR_EN: u32 = 1 << 16;
pub const LNA_PE_G1_EN: u32 = 1 << 11;
pub const LNA_PE_A1_EN: u32 = 1 << 10;
pub const LNA_PE_G0_EN: u32 = 1 << 9;
pub const LNA_PE_A0_EN: u32 = 1 << 8;
pub const PA_PE_G1_EN: u32 = 1 << 3;
pub const PA_PE_G0_EN: u32 = 1 << 1;
// TX_BAND_CFG
pub const BAND_SEL_5G_N: u32 = 1 << 2;
pub const BAND_SEL_5G_P: u32 = 1 << 1;
// TX_SW_CFG0
pub const DLY_PAPE_EN_SHIFT: u32 = 8;
// RX_FILTR_CFG
pub const DROP_CTRL_RSV: u32 = 1 << 16;
pub const DROP_BAR: u32 = 1 << 15;
pub const DROP_BA: u32 = 1 << 14;
pub const DROP_PSPOLL: u32 = 1 << 13;
pub const DROP_RTS: u32 = 1 << 12;
pub const DROP_CTS: u32 = 1 << 11;
pub const DROP_ACK: u32 = 1 << 10;
pub const DROP_CFEND: u32 = 1 << 9;
pub const DROP_CFACK: u32 = 1 << 8;
pub const DROP_DUPL: u32 = 1 << 7;
/// Multicast (group addresses other than broadcast).
pub const DROP_MC: u32 = 1 << 5;
pub const DROP_VER_ERR: u32 = 1 << 4;
/// Frames of other networks (BSSID not MAC_BSSID_DW0/1).
pub const DROP_NOT_MY_BSSD: u32 = 1 << 3;
/// Unicast frames not addressed to us.
pub const DROP_UC_NOME: u32 = 1 << 2;
pub const DROP_PHY_ERR: u32 = 1 << 1;
pub const DROP_CRC_ERR: u32 = 1 << 0;
// AUTO_RSP_CFG (the automatic ACK/CTS responder)
/// ACK/CTS at CCK rates with a short preamble.
pub const CCK_SHORT_EN: u32 = 1 << 4;
pub const AUTO_RSP_EN: u32 = 1 << 0;
// BKOFF_SLOT_CFG: the slot time (µs) in the low byte.
pub const SLOT_TIME_MASK: u32 = 0xff;
// H2M_MAILBOX
pub const H2M_BUSY: u32 = 1 << 24;
pub const TOKEN_NO_INTR: u32 = 0xff;

// MCU commands
pub const MCU_CMD_LEDS: u8 = 0x50;
pub const MCU_CMD_LED1: u8 = 0x52;
pub const MCU_CMD_LED2: u8 = 0x53;
pub const MCU_CMD_LED3: u8 = 0x54;
/// MCU_BOOT_SIGNAL in Linux.
pub const MCU_CMD_RFRESET: u8 = 0x72;
/// The RF frequency-offset command (`run_adjust_freq_offset`).
pub const MCU_CMD_FREQ_OFFSET: u8 = 0x74;
pub const LED_RADIO: u16 = 1 << 13;
pub const LED_LINK_2GHZ: u16 = 1 << 14;

// RF registers' bits (RFCSR)
pub const RF1_RF_BLOCK: u8 = 1 << 0;
pub const RF1_PLL_PD: u8 = 1 << 1;
pub const RF1_RX0_PD: u8 = 1 << 2;
pub const RF1_TX0_PD: u8 = 1 << 3;
/// RF R2: resistor calibration.
pub const RF2_RESCAL: u8 = 1 << 7;
/// RF R3: VCO calibration.
pub const RF3_VCOCAL: u8 = 1 << 7;
pub const RF38_RX_LO1: u8 = 1 << 5;
pub const RF39_RX_LO2: u8 = 1 << 7;
// BBP registers' bits
pub const BBP4_MAC_IF_CTRL: u8 = 1 << 6;
pub const BBP105_MLD: u8 = 1 << 2;

// TX descriptors (`struct rt2870_txd` and `struct rt2860_txwi`)
/// TXINFO flags byte: the chip's TX queue: EDCA (the WMM queues).
pub const TXINFO_QSEL_EDCA: u8 = 2 << 1;
/// TXINFO flags byte: "wireless info valid", the IV words in TXWI are
/// the driver's: the CCMP header for a frame the chip encrypts (a WCID
/// with a key), else zero. Without it the chip takes the header from its
/// IVEIV table.
pub const TXINFO_WIV: u8 = 1 << 0;
/// TXWI txop: a normal backoff before sending.
pub const TXWI_TXOP_BACKOFF: u8 = 3;
/// TXWI PHY word: the modulation, short preamble (CCK), the rate.
pub const PHY_CCK: u16 = 0 << 14;
pub const PHY_OFDM: u16 = 1 << 14;
/// HT mixed mode (a legacy preamble first, so 802.11g stations defer).
pub const PHY_HT_MIX: u16 = 2 << 14;
pub const PHY_SHPRE: u16 = 1 << 3;
/// HT: the short (400 ns) guard interval.
pub const PHY_SGI: u16 = 1 << 8;
// MM20_PROT_CFG and GF20_PROT_CFG: the protection of HT frames (Linux's
// rt2800.h): the rate (PHY word) of the RTS/CTS in the low half, the
// mode in bits 16-17.
pub const PROT_RATE_MASK: u32 = 0xffff;
pub const PROT_CTRL_MASK: u32 = 3 << 16;
/// RTS/CTS before each HT frame.
pub const PROT_CTRL_RTS_CTS: u32 = 1 << 16;
/// TXWI flags: the chip writes the TSF into the frame's timestamp (beacons).
pub const TXWI_TS: u8 = 1 << 3;
/// TXWI txop: HT (no backoff; what rt2860.c's beacons have).
pub const TXWI_TXOP_HT: u8 = 0;
/// Beacon slot 0 in the chip's memory (`RT2860_BCN_BASE(0)`; BCN_OFFSET0's
/// low byte 0xe0 × 64 from 0x4000), 512 bytes.
pub const BCN_BASE0: u16 = 0x7800;
pub const BCN_SIZE: usize = 512;
/// TXWI xflags: the chip assigns the sequence number.
pub const TXWI_NSEQ: u8 = 1 << 1;
/// TXWI xflags: wait for an ACK (and retry without one).
pub const TXWI_ACK: u8 = 1 << 0;
/// TXWI WCID: no entry in the chip's station/key table.
pub const WCID_NONE: u8 = 0xff;
/// The station-table entry for the access point's pairwise key (PKEY).
/// OpenBSD uses the AID; any entry but 0xff works, as long as TXWI and
/// the tables agree.
pub const WCID_AP: u8 = 1;
/// WCID_ATTR bit 0: the entry's key is in the pairwise table (PKEY), not
/// the shared one (SKEY). OpenBSD calls it `RT2860_RX_PKEY_EN`, Linux
/// `KEYTAB`; it applies to sending as well (clear, the chip encrypts
/// with the empty shared key: seen on the Toon). Bits 1-3: the cipher.
pub const WCID_ATTR_PKEY: u32 = 1 << 0;
pub const MODE_AES_CCMP: u32 = 4;

// RX descriptor (RXD, the 32-bit word after each frame) flags
/// The chip decrypted the frame (`RT2860_RX_DEC`).
pub const RXD_DEC: u32 = 1 << 16;
pub const RXD_L2PAD: u32 = 1 << 14;
pub const RXD_MICERR: u32 = 1 << 10;
pub const RXD_ICVERR: u32 = 1 << 9;
pub const RXD_CRCERR: u32 = 1 << 8;

// RF revisions (EEPROM word 0 on the RT5390 family)
pub const RF_5370: u16 = 0x5370;
pub const RF_5390: u16 = 0x5390;

// ---- EEPROM / efuse word addresses ----
pub const EEPROM_CHIPID: u16 = 0x00;
pub const EEPROM_VERSION: u16 = 0x01;
pub const EEPROM_MAC01: u16 = 0x02;
pub const EEPROM_ANTENNA: u16 = 0x1a;
pub const EEPROM_CONFIG: u16 = 0x1b;
pub const EEPROM_FREQ_LEDS: u16 = 0x1d;
pub const EEPROM_LED1: u16 = 0x1e;
pub const EEPROM_LNA: u16 = 0x22;
pub const EEPROM_RSSI1_2GHZ: u16 = 0x23;
pub const EEPROM_PWR2GHZ_BASE1: u16 = 0x29;
pub const EEPROM_RPWR: u16 = 0x6f;
pub const EEPROM_BBP_BASE: u16 = 0x78;

/// The MAC registers' defaults (`RT2870_DEF_MAC`). Receive only,
/// AUTO_RSP_CFG is written as 0 afterwards (no automatic ACK/CTS).
pub const DEF_MAC: &[(u16, u32)] = &[
    (BCN_OFFSET0, 0xf8f0e8e0),
    (LEGACY_BASIC_RATE, 0x0000013f),
    (HT_BASIC_RATE, 0x00008003),
    (MAC_SYS_CTRL, 0x00000000),
    (BKOFF_SLOT_CFG, 0x00000209),
    (TX_SW_CFG0, 0x00000000),
    (TX_SW_CFG1, 0x00080606),
    (TX_LINK_CFG, 0x00001020),
    (TX_TIMEOUT_CFG, 0x000a2090),
    (LED_CFG, 0x7f031e46),
    (WMM_AIFSN_CFG, 0x00002273),
    (WMM_CWMIN_CFG, 0x00002344),
    (WMM_CWMAX_CFG, 0x000034aa),
    (MAX_PCNT, 0x1f3fbf9f),
    (TX_RTY_CFG, 0x47d01f0f),
    (AUTO_RSP_CFG, 0x00000013),
    (CCK_PROT_CFG, 0x05740003),
    (OFDM_PROT_CFG, 0x05740003),
    (PBF_CFG, 0x00f40006),
    (WPDMA_GLO_CFG, 0x00000030),
    (GF20_PROT_CFG, 0x01744004),
    (GF40_PROT_CFG, 0x03f44084),
    (MM20_PROT_CFG, 0x01744004),
    (MM40_PROT_CFG, 0x03f44084),
    (TXOP_CTRL_CFG, 0x0000583f),
    (TXOP_HLDR_ET, 0x00000002),
    (TX_RTS_CFG, 0x00092b20),
    (EXP_ACK_TIME, 0x002400ca),
    (XIFS_TIME_CFG, 0x33a41010),
    (PWR_PIN_CFG, 0x00000003),
];

/// The BBP registers' defaults for the RT5390 family (`RT5390_DEF_BBP`).
pub const RT5390_DEF_BBP: &[(u8, u8)] = &[
    (31, 0x08),
    (65, 0x2c),
    (66, 0x38),
    (68, 0x0b),
    (69, 0x0d),
    (70, 0x06),
    (73, 0x13),
    (75, 0x46),
    (76, 0x28),
    (77, 0x59),
    (81, 0x37),
    (82, 0x62),
    (83, 0x7a),
    (84, 0x9a),
    (86, 0x38),
    (91, 0x04),
    (92, 0x02),
    (103, 0xc0),
    (104, 0x92),
    (105, 0x3c),
    (106, 0x03),
    (128, 0x12),
];

/// The RF registers' defaults for the RT5390/RF5370 (`RT5390_DEF_RF`).
pub const RT5390_DEF_RF: &[(u8, u8)] = &[
    (1, 0x0f),
    (2, 0x80),
    (3, 0x88),
    (5, 0x10),
    (6, 0xa0),
    (7, 0x00),
    (10, 0x53),
    (11, 0x4a),
    (12, 0x46),
    (13, 0x9f),
    (14, 0x00),
    (15, 0x00),
    (16, 0x00),
    (18, 0x03),
    (19, 0x00),
    (20, 0x00),
    (21, 0x00),
    (22, 0x20),
    (23, 0x00),
    (24, 0x00),
    (25, 0xc0),
    (26, 0x00),
    (27, 0x09),
    (28, 0x00),
    (29, 0x10),
    (30, 0x10),
    (31, 0x80),
    (32, 0x80),
    (33, 0x00),
    (34, 0x07),
    (35, 0x12),
    (36, 0x00),
    (37, 0x08),
    (38, 0x85),
    (39, 0x1b),
    (40, 0x0b),
    (41, 0xbb),
    (42, 0xd2),
    (43, 0x9a),
    (44, 0x0e),
    (45, 0xa2),
    (46, 0x7b),
    (47, 0x00),
    (48, 0x10),
    (49, 0x94),
    (52, 0x38),
    (53, 0x84),
    (54, 0x78),
    (55, 0x44),
    (56, 0x22),
    (57, 0x80),
    (58, 0x7f),
    (59, 0x8f),
    (60, 0x45),
    (61, 0xdd),
    (62, 0x00),
    (63, 0x00),
];

/// The synthesizer settings for channels 1-14 (`RT3070_RF3052`, the
/// 2.4 GHz part; Linux's `rf_vals_3x` has the same numbers): (N, R, K).
/// RF R8 = N, R9 = K, R11[1:0] = R. With the RT5370's 40 MHz crystal.
pub const FREQS_2GHZ: [(u8, u8, u8); 14] = [
    (0xf1, 2, 2),
    (0xf1, 2, 7),
    (0xf2, 2, 2),
    (0xf2, 2, 7),
    (0xf3, 2, 2),
    (0xf3, 2, 7),
    (0xf4, 2, 2),
    (0xf4, 2, 7),
    (0xf5, 2, 2),
    (0xf5, 2, 7),
    (0xf6, 2, 2),
    (0xf6, 2, 7),
    (0xf7, 2, 2),
    (0xf8, 2, 4),
];
