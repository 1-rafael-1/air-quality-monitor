# Air Quality Monitor

A compact, battery-powered air quality monitoring device built with Rust and a Waveshare RP2350-based board. This device continuously monitors air quality, temperature, and humidity with a small OLED display.

**⚠️ Educational Project Notice**: This is a hobby project developed for educational purposes only.

## How It Works

The device operates on a low-power cycle to maximize battery life:

1. **Sensor Reading**: Collects data from ENS160 (air quality) and AHT21 (temperature/humidity) sensors every few minutes
2. **Data Processing**: Uses median filtering on air quality readings to reduce noise
3. **Display Updates**: Shows current readings and battery status on a 128x64 OLED display
4. **Power Management**: Automatically enters idle modes between readings and monitors battery voltage
5. **Mode Switching**: Automatically cycles between sensor data and CO2 history views every 10 seconds

## Enclosure

The device features a custom 3D-printed enclosure designed for portability and sensor access.

**3D Print Files**: All enclosure files are available in the [`enclosure/`](./enclosure/) directory:

- `Main Body.3mf` - Main housing with sensor openings
- `Back Lid.3mf` - Battery compartment cover
- `EnclosureComplete.FCStd` - Complete FreeCAD project file

<table>
<tr>
<td><img src="enclosure/enclosure.png" alt="Enclosure" width="400"/></td>
<td><img src="enclosure/lid.png" alt="Lid Detail" width="186"/></td>
</tr>
</table>

## Components

| Component | Description | Purpose |
|-----------|-------------|---------|
| Waveshare RP2350 Board | RP2350-based development board. There are cheaper alternatives available on Aliexpress, so lomng as they have the Pico 2 form factor anda battery connector with charger it should be fine | Core processing and I/O |
| ENS160 + AHT21 Module | Combined air quality and temperature/humidity sensor board. There are cheap combined boards available | Environmental monitoring |
| SSD1306 | 128x64 OLED Display, yellow/blue in my case here but monochrome or blue will work just as well| Data visualization |
| LiPo Battery | 3.7V rechargeable battery. I use 2500mA, 7 x 40 x 60mm with a 1.25mm connector | Portable power source |

## Hardware Connections

Connect the components to the Waveshare RP2350 board as follows:

### I2C Bus (Shared)

All I2C devices (ENS160 + AHT21 module and SSD1306 display) connect to the same I2C bus.
Since ENS160 and AHT21 are on the same module, they share the bus anyway and the  SSD1306 display is also connected to the same I2C bus for simplicity.

### ENS160 + AHT21 Module

- **VCC**: 3.3V
- **GND**: Ground
- **SDA**: GPIO 16
- **SCL**: GPIO 17
- **INT**: GPIO 18 (ENS160 interrupt pin)

### SSD1306 OLED Display

- **VCC**: 3.3V
- **GND**: Ground
- **SDA**: GPIO 16
- **SCL**: GPIO 17

### Power Monitoring

Both of these are already connected on the Waveshare board, no additional wiring is needed.

- **VBUS**: GPIO 24 (USB power detection)
- **VSYS**: GPIO 29 (Battery voltage monitoring via ADC)

### Battery

- **Battery**: 3.7V LiPo battery connected to the battery connector on the Waveshare board.

The Waveshare board has built-in battery charging and power management, so no additional components are required.
In case you want to use i.e. a Pi Pico 2, You need to add a charger board and appropriate circuit. The Pi Pico 2 datasheet has instructions.

## Code Structure

The firmware is built using Embassy (async Rust) and organized into modular tasks:

```text
src/
├── main.rs          # Entry point, hardware initialization, task spawning
├── sensor.rs        # ENS160 and AHT21 sensor data acquisition
├── display.rs       # SSD1306 OLED display management and UI rendering
├── event.rs         # Inter-task communication events
├── orchestrate.rs   # Main control loop and data coordination
├── vbus.rs          # USB power detection
├── vsys.rs          # Battery voltage monitoring
├── watchdog.rs      # System watchdog
└── media/           # Bitmap assets for display (battery icons, etc.)
```

### Key Features

- **Async Architecture**: Uses Embassy framework for task scheduling
- **Power Optimization**: 18MHz clock, voltage scaling, and idle modes
- **Median Filtering**: Reduces sensor noise through statistical processing
- **Battery Monitoring**: Real-time voltage tracking with visual indicators
- **Mode Switching**: Automatic display cycling between sensor data and CO2 history views

## Building and Flashing

```bash
# Build for release (optimized for size and power)
cargo build --release

# Option 1: Flash directly with picotool (elf2uf2-rs does not support RP2350 as of 06.2025)
# Put board in bootloader mode (hold BOOTSEL while connecting USB)
picotool load -u -v -x -t elf target/thumbv8m.main-none-eabihf/release/air-quality-monitor

# Option 2: Convert to UF2 and copy manually
picotool uf2 convert target/thumbv8m.main-none-eabihf/release/air-quality-monitor -t elf air-quality-monitor.uf2 -t uf2
# Copy the resulting .uf2 file to the RP2350 board in bootloader mode
```

For development builds probe-rs can be used:

```bash
# Build for development
cargo build
# Flash using probe-rs
cargo run
```

## License

This project is licensed under either of:

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
