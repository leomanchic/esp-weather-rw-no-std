#![no_std]
#![no_main]

use chrono::{DateTime, Datelike, Duration, TimeZone, Timelike, Utc};
use core::{fmt::Write, ops::Add};
use embedded_svc::{
    ipv4::Interface,
    wifi::{AccessPointInfo, ClientConfiguration, Configuration, Wifi},
};
use hal::{
    clock::ClockControl, gpio::IO, i2c::I2C, peripherals::Peripherals, prelude::*,
    spi::SpiDataMode, Delay, Rng, Rtc,
};
use heapless::String;

use esp_backtrace as _;
use esp_println::println;
use esp_wifi::{
    current_millis, initialize,
    wifi::{utils::create_network_interface, WifiError, WifiStaDevice},
    wifi_interface::WifiStack,
    EspWifiInitFor,
};
use smoltcp::{
    iface::SocketStorage,
    wire::{IpAddress, Ipv4Address},
};

use embedded_graphics::{
    mono_font::{ascii::FONT_6X10, MonoTextStyleBuilder},
    pixelcolor::BinaryColor,
    prelude::*,
    text::{Baseline, Text},
};
use ssd1306::{prelude::*, I2CDisplayInterface, Ssd1306};

const SSID: &str = "EVA";
const PASSWORD: &str = "GALA2015";

#[entry]
fn main() -> ! {
    let peripherals = Peripherals::take();
    let mut system = peripherals.SYSTEM.split();

    // Configure Wifi
    let wifi = peripherals.WIFI;
    let mut socket_set_entries: [SocketStorage; 3] = Default::default();
    let rtc = Rtc::new(peripherals.RTC_CNTL);
    let clocks = ClockControl::max(system.clock_control).freeze();
    let timer = hal::timer::TimerGroup::new(peripherals.TIMG1, &clocks).timer0;
    let init = initialize(
        EspWifiInitFor::Wifi,
        timer,
        Rng::new(peripherals.RNG),
        system.radio_clock_control,
        &clocks,
    )
    .unwrap();
    let (iface, device, mut controller, sockets) =
        create_network_interface(&init, wifi, WifiStaDevice, &mut socket_set_entries).unwrap();
    let wifi_stack = WifiStack::new(iface, device, sockets, current_millis);
    let client_config = Configuration::Client(ClientConfiguration {
        ssid: SSID.into(),
        password: PASSWORD.into(),
        ..Default::default()
    });

    //Configure Display
    let io = IO::new(peripherals.GPIO, peripherals.IO_MUX);

    // Create a new peripheral object with the described wiring
    // and standard I2C clock speed
    let i2c = I2C::new(
        peripherals.I2C0,
        io.pins.gpio32,
        io.pins.gpio33,
        100u32.kHz(),
        &clocks,
    );

    let interface = I2CDisplayInterface::new(i2c);
    let mut display = Ssd1306::new(interface, DisplaySize128x32, DisplayRotation::Rotate0)
        .into_buffered_graphics_mode();
    display.init().unwrap();

    // text styles
    let text_style = MonoTextStyleBuilder::new()
        .font(&FONT_6X10)
        .text_color(BinaryColor::On)
        .build();

    // let text_style_big = MonoTextStyleBuilder::new()
    //     .font(&FONT_6X10)
    //     .text_color(BinaryColor::On)
    //     .build();

    // Create a Client with your Wi-Fi credentials and default configuration.
    // let client_config = Configuration::Client();
    let res = controller.set_configuration(&client_config);
    println!("wifi_set_configuration returned {:?}", res);

    // Start Wi-Fi controller, scan the available networks.
    controller.start().unwrap();
    println!("is wifi started: {:?}", controller.is_started());

    println!("Start Wifi Scan");

    let res: Result<(heapless::Vec<AccessPointInfo, 10>, usize), WifiError> = controller.scan_n();
    if let Ok((res, _count)) = res {
        for ap in res {
            println!("{:?}", ap);
        }
    }

    println!("{:?}", controller.get_capabilities());
    println!("wifi_connect {:?}", controller.connect());

    // Wait to get connected
    println!("Wait to get connected");
    loop {
        let res = controller.is_connected();
        match res {
            Ok(connected) => {
                if connected {
                    break;
                }
            }
            Err(err) => {
                println!("{:?}", err);
                loop {}
            }
        }
    }
    println!("{:?}", controller.is_connected());

    // Wait for getting an ip address
    println!("Wait to get an ip address");
    loop {
        wifi_stack.work();

        if wifi_stack.is_iface_up() {
            println!("got ip {:?}", wifi_stack.get_ip_info());
            break;
        }
    }

    println!("Start busy loop on main");
    let mut delay = Delay::new(&clocks);

    let mut rx_buffer = [0u8; 1536];
    let mut tx_buffer = [0u8; 1536];
    let mut socket = wifi_stack.get_socket(&mut rx_buffer, &mut tx_buffer);

    println!("GET NTP TIME");
    let mut rx_meta1 = [smoltcp::socket::udp::PacketMetadata::EMPTY; 10];
    let mut rx_buffer1 = [0u8; 1536];
    let mut tx_meta1 = [smoltcp::socket::udp::PacketMetadata::EMPTY; 10];
    let mut tx_buffer1 = [0u8; 1536];
    let mut udp_socket = wifi_stack.get_udp_socket(
        &mut rx_meta1,
        &mut rx_buffer1,
        &mut tx_meta1,
        &mut tx_buffer1,
    );
    udp_socket.bind(50123).unwrap();

    let req_data = ntp_nostd::get_client_request();
    let mut rcvd_data = [0_u8; 1536];
    udp_socket
        // using ip from https://tf.nist.gov/tf-cgi/servers.cgi (time-a-g.nist.gov)
        .send(Ipv4Address::new(129, 6, 15, 28).into(), 123, &req_data)
        .unwrap();
    let mut count = 0;

    // get time from ntp server. requires delaying because UDP packets can arrive whenever
    let rtc_offset: u64; // = 0;
    let unix_time: u32; // = 0;
    loop {
        count += 1;
        let rcvd = udp_socket.receive(&mut rcvd_data);
        if rcvd.is_ok() {
            // set global static offset variable
            rtc_offset = rtc.get_time_ms();
            break;
        }

        // delay to wait for data to show up to port
        delay.delay_ms(500_u32);

        if count > 10 {
            udp_socket
                // retry with another server
                // using ip from https://tf.nist.gov/tf-cgi/servers.cgi (time-b-g.nist.gov)
                .send(Ipv4Address::new(129, 6, 15, 29).into(), 123, &req_data)
                .unwrap();
            println!("reset ntp count...");
            count = 0;
        }
    }
    let response = ntp_nostd::NtpServerResponse::from(rcvd_data.as_ref());
    if response.headers.tx_time_seconds == 0 {
        panic!("No timestamp received");
    }
    unix_time = response.headers.get_unix_timestamp();
    // let timer = embedded_svc::sys_time::SystemTime::now(&self);
    // let human_readable = get_utc_timestamp(&rtc, unix_time, rtc_offset);

    println!("Unix time: {} ", unix_time);

    // let human_readable: DateTime<Utc> = Utc.timestamp_opt(unix_time as i64, 0).unwrap();

    // println!("Time {} {}", human_readable, rtc.get_time_raw());

    loop {
        // println!("Making HTTP request");
        let human_readable = get_utc_timestamp(&rtc, unix_time, rtc_offset);
        let human_readable: DateTime<Utc> = Utc
            .timestamp_opt((human_readable + 10800) as i64, 0)
            .unwrap();

        let mut buffer: String<32> = heapless::String::new();
        write!(
            buffer,
            "{:04}-{:02}-{:02} T{:02}:{:02}:{:02}",
            human_readable.year(),
            human_readable.month(),
            human_readable.day(),
            human_readable.hour(),
            human_readable.minute(),
            human_readable.second()
        )
        .unwrap();
        Text::with_baseline(
            // &format!("Moscow, Russia:{}", resp_weather),
            &buffer,
            Point::new(0, 0),
            text_style,
            Baseline::Top,
        )
        .draw(&mut display)
        .unwrap();
        display.flush().unwrap();
        display.clear(BinaryColor::Off).unwrap();

        delay.delay_ms(1000u32);
    }
}
fn get_utc_timestamp(rtc: &Rtc, unix_time: u32, rtc_offset: u64) -> u32 {
    let time_now = rtc.get_time_ms() / 1000;
    let rtc_offset_s = rtc_offset / 1000;
    unix_time + u32::try_from(time_now - rtc_offset_s).unwrap()
}
