# Air Quality Monitor

A compact, battery-powered air quality monitoring device built with Rust and a Waveshare RP2350-based board. This device continuously monitors air quality, temperature, and humidity with a small OLED display.

**⚠️ Educational Project Notice**: This is a hobby project developed for educational purposes only.

## Finished Device

<table>
<tr>
<td><img src="readme_media/finished_device.png" alt="Finished Air Quality Monitor Device" width="400"/></td>
<td><img src="readme_media/finished_device_values.png" alt="Device Display Showing Values" width="400"/></td>
</tr>
</table>

## How It Works

1. **Sensor Reading**: Collects data from ENS160 (air quality) and AHT21 (temperature/humidity) sensors every 5 minutes
2. **Data Processing**: Uses median filtering on air quality readings to reduce noise
3. **Display Updates**: Shows current readings and battery status on a 128x64 OLED display, changing between data and bar graph views every 10 seconds
4. **Power Management**: Reduced clock speed (18MHz) and core voltage to conserve power.
5. **Battery Monitoring**: VSYS voltage is measured every 4 seconds to determine battery level and charging state. Uses moving median filtering (5 samples) when on battery power for stable readings, and direct measurements when charging to reduce latency.

## Components

| Component | Description | Purpose |
|-----------|-------------|---------|
| Waveshare RP2350 Board | RP2350-based development board. There are cheaper alternatives available on AliExpress, so long as they have the Pico 2 form factor and a battery connector with charger it should be fine | Core processing and I/O |
| ENS160 + AHT21 Module | Combined air quality and temperature/humidity sensor board. There are cheap combined boards available | Environmental monitoring |
| SSD1306 | 128x64 OLED Display, yellow/blue in my case here but monochrome or blue will work just as well | Data visualization |
| LiPo Battery | 3.7V rechargeable battery. I use 2500mAh, 7 x 40 x 60mm with a 1.25mm connector | Portable power source |
| Slide Switch | Slide switch to control power | Power management |

### Things to know

The ENS160 sensor and AHT21 sensor are on a combined board here, bought very cheap. An ENS160 datasheet and the ENS160 sensor on the board do not always agree on things. The AHT21 may in fact be an AHT20 according to some comments...

Here is what I found not being what I expected:

+ ENS160 ADD pin seems to be floating, sensor will not work if not connected to GND
+ Vendor says 1min warmup time, datasheet says 3min warmup time for ENS160.
+ ENS160 datasheet specifies `InitialStartupPhase` during the first 1 hour of operation and after 24h the sensor should save that it will never need this again. I find this not to be the case, that way all ideas of elaborate power management and sleep/wake cycles are out of the window. The sensor needs to be continuously powered to provide reliable readings.
+ AHT21 humidity is always reasonably close to external sensor readings, but temperature is always 2 to 3 degrees Celsius above external sensor readings. I just introduced a correction factor in code. I suppose this is due to the sensor being on the same board as the ENS160, which generates heat during operation.

Bottom line: You buy cheap, you get cheap.

## Hardware Connections

Connect the components to the Waveshare RP2350 board as follows:

### I2C Bus (Shared)

All I2C devices (ENS160 + AHT21 module and SSD1306 display) connect to the same I2C bus.
Since ENS160 and AHT21 are on the same module, they share the bus anyway and the SSD1306 display is also connected to the same I2C bus for simplicity.

### ENS160 + AHT21 Module

+ **VCC**: 3.3V
+ **GND**: Ground
+ **SDA**: GPIO 16
+ **SCL**: GPIO 17
+ **INT**: GPIO 18 (ENS160 interrupt pin)

### SSD1306 OLED Display

+ **VCC**: 3.3V
+ **GND**: Ground
+ **SDA**: GPIO 16
+ **SCL**: GPIO 17

### Power Monitoring

The Waveshare board has a battery connector, that is wired to vsys, so not need for connections beside plugging in the battery.

Power and charging detection is handled through VSYS voltage monitoring due to RP2350 E9 erratum affecting VBUS detection.

### Battery

+ **Battery**: 3.7V LiPo battery connected to the battery connector on the Waveshare board.

The Waveshare board has built-in battery charging and power management, so no additional components are required.
In case you want to use i.e. a Pi Pico 2, you need to add a charger board and appropriate circuit. The Pi Pico 2 datasheet has instructions.

## Power Consumption

On average I measure around 39mA current consumption when supplying 3.7V to the device (which is the nominal voltage for the battery I use).

The baseline for controller and display combined is around 10-11mA, the ENS160 sensor draws around 28mA when operating continuously in Standard mode, the AHT21 is negligible. With these values the device can theoretically run for around 65 hours on a 2500mAh battery, considering the built-in charge controller of the battery will not let us use more than say 90% of the battery capacity.

Sleep/wake cycling was initially attempted to reduce power consumption, but proved unreliable (see above).

## Assembly and Enclosure

### Electronics Assembly

Wire the components according to the hardware connections above. For the slide switch, cut the ground wire of the battery connector and connect it to the switch.

<table>
<tr>
<td><img src="readme_media/assembly_electronics.png" alt="Electronics Assembly" width="400"/></td>
</tr>
</table>

### 3D-Printed Enclosure

The device features a custom 3D-printed enclosure.

**3D Print Files**: All enclosure files are available in the [`enclosure/`](./enclosure/) directory:

+ `Main Body.3mf` - Main housing with sensor openings
+ `Back Lid.3mf` - Battery compartment cover
+ `EnclosureComplete.FCStd` - Body FreeCAD file
+ `Back Lid.FCStd` - Back lid FreeCAD file

<table>
<tr>
<td><img src="enclosure/enclosure.png" alt="3D Printed Enclosure" width="400"/></td>
<td><img src="enclosure/lid.png" alt="Enclosure Lid Detail" width="186"/></td>
</tr>
</table>

### Enclosure Assembly

After printing the enclosure parts, assemble the electronics into the housing. The main body provides openings for sensor airflow while protecting the internal components.

<table>
<tr>
<td><img src="readme_media/assembly_enclosure.png" alt="Enclosure Assembly Process" width="400"/></td>
</tr>
</table>

Use brass hot melt inserts for the enclosure screws to secure the back lid. Same goes for the slide switch, which is mounted on the front side of the enclosure.

The whole electronics can be fitted inside without any glue, only the OLED display needs held in place by tape (because i forgot to design a holder for it).

<table>
<tr>
<td><img src="readme_media/assembly_electronics_in_enclosure.png" alt="Enclosure Assembly Process 2" width="400"/></td>
</tr>
</table>

## Code Structure

The firmware is built using Embassy (async Rust) and organized into modular tasks:

```text
src/
├── main.rs          # Entry point, hardware initialization, task spawning
├── sensor.rs        # ENS160 and AHT21 sensor data acquisition
├── display.rs       # SSD1306 OLED display management and UI rendering
├── event.rs         # Inter-task communication events
├── orchestrate.rs   # Main control loop and data coordination
├── system_state.rs  # System state management (battery, sensor data, display modes)
├── vsys.rs          # Battery voltage monitoring and charging detection
├── watchdog.rs      # System watchdog
└── media/           # Bitmap assets for display (battery icons, etc.)
```

### Key Features

+ **Async Architecture**: Uses Embassy framework for task scheduling
+ **Power Optimization**: 18MHz clock, voltage scaling, and idle modes
+ **Median Filtering**: Reduces sensor noise through statistical processing
+ **Battery Monitoring**: VSYS-based voltage tracking with adaptive filtering (median filtering on battery, direct measurement when charging)
+ **Charging Detection**: Automatic detection of charging state via voltage thresholds (works around RP2350 E9 erratum)
+ **Mode Switching**: Automatic display cycling between sensor data and CO2 history views
+ **Watchdog System**: Monitors task health with 15-minute timeout and automatic system reset on failure

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

+ [Apache License, Version 2.0](LICENSE-APACHE)
+ [MIT License](LICENSE-MIT)

at your option.
