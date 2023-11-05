use anyhow::Result;
use cairo::{Context, FontSlant, FontWeight, Format, ImageSurface, Rectangle};
use chrono::Local;
use drm::control::ClipRect;
use icon_loader::{IconFileType, IconLoader};
use image::{
        DynamicImage, Pixel,
        imageops::{resize, FilterType},
};
use input::{
    event::{
        device::DeviceEvent,
        keyboard::{KeyState, KeyboardEvent, KeyboardEventTrait},
        touch::{TouchEvent, TouchEventPosition, TouchEventSlot},
        Event, EventTrait,
    },
    Device as InputDevice, Libinput, LibinputInterface,
};
use input_linux::{uinput::UInputHandle, EventKind, Key, SynchronizeKind};
use input_linux_sys::{input_event, input_id, timeval, uinput_setup};
use libc::{c_char, O_ACCMODE, O_RDONLY, O_RDWR, O_WRONLY};
use nix::poll::{poll, PollFd, PollFlags};
use privdrop::PrivDrop;
use rsvg::{CairoRenderer, Loader, SvgHandle};
use serde::Deserialize;
use lazy_static::lazy_static;
use std::{
    collections::HashMap,
    fs::{read_to_string, self, File, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::{fs::OpenOptionsExt, io::OwnedFd},
    },
    path::{Path, PathBuf},
    time::{SystemTime},
};

mod backlight;
mod display;

use backlight::BacklightManager;
use display::DrmBackend;

const BUTTON_COLOR_INACTIVE: f64 = 0.200;
const BUTTON_COLOR_ACTIVE: f64 = 0.400;
const TIMEOUT_MS: i32 = 30 * 1000;
const CONFIG_PATH: &str = "/etc/tiny-dfr.conf";

enum ButtonImage {
    Text(String),
    Svg(SvgHandle),
    Png(DynamicImage),
    Time(u16),
    Blank,
}

struct Button {
    image: ButtonImage,
    changed: bool,
    active: bool,
    action: Key,
}

impl Button {
    fn new_text(text: &str, action: Key) -> Button {
        Button {
            action,
            active: false,
            changed: false,
            image: ButtonImage::Text(text.to_string()),
        }
    }
    fn new_icon(icon_name: &str, action: Key, icon_theme: &str) -> Button {
        let mut search_paths: Vec<PathBuf> = vec![
            PathBuf::from("/usr/share/tiny-dfr/icons/"),
            PathBuf::from("/usr/share/icons/"),
        ];
        let mut loader = IconLoader::new();
        search_paths.extend(loader.search_paths().into_owned());
        loader.set_search_paths(search_paths);
        loader.set_theme_name_provider(icon_theme);
        loader.update_theme_name().unwrap();
        let image;
        match loader.load_icon(icon_name) {
            Some(icon_loader) => {
                let icon = icon_loader.file_for_size(512);
                match icon.icon_type() {
                    IconFileType::SVG => {
                        image = ButtonImage::Svg(Loader::new().read_path(icon.path()).unwrap());
                    }
                    IconFileType::PNG => {
                        image = ButtonImage::Png(image::open(icon.path()).unwrap());
                    }
                    IconFileType::XPM => {
                        panic!("Legacy XPM icons are not supported")
                    }
                }
            }       
            None => {
                // If loading the icon from the theme fails, try /usr/share/pixmaps

                let icon_path_svg = Path::new("/usr/share/pixmaps").join(format!("{}.svg", icon_name));
                let icon_path_png = Path::new("/usr/share/pixmaps").join(format!("{}.png", icon_name));


                if icon_path_svg.exists() {
                        image = ButtonImage::Svg(Loader::new().read_path(icon_path_svg).unwrap());
                } else if icon_path_png.exists() {
                        image = ButtonImage::Png(image::open(icon_path_png).unwrap());
                } else {
                    // If the icon is not found in /usr/share/pixmaps, use the icon_name as text
                        image = ButtonImage::Text(icon_name.to_string());
                }
            }
        };
        Button {
            action,
            active: false,
            changed: false,
            image,
        }
    }
    fn new_time(use_24_hour: u16) -> Button {
        Button {
            action: Key::Time,
            active: false,
            changed: false,
            image: ButtonImage::Time(use_24_hour),
        }
    }
    fn new_blank() -> Button {
        Button {
            action: Key::Unknown,
            active: false,
            changed: false,
            image: ButtonImage::Blank,
        }
    }
    fn render(&self, c: &Context, height: f64, left_edge: f64, button_width: f64) {
        match &self.image {
            ButtonImage::Text(text) => {
                let extents = c.text_extents(text).unwrap();
                c.move_to(
                    left_edge + button_width / 2.0 - extents.width() / 2.0,
                    height / 2.0 + extents.height() / 2.0,
                );
                c.show_text(text).unwrap();
            },
            ButtonImage::Svg(svg) => {
                let renderer = CairoRenderer::new(&svg);
                let y = 0.10 * height;
                let size = height - y * 2.0;
                let x = left_edge + button_width / 2.0 - size / 2.0;
                renderer
                    .render_document(c, &Rectangle::new(x, y, size, size))
                    .unwrap();
            },
            ButtonImage::Png(png) => {
                let y = 0.10 * height;
                let size = height - y * 2.0;
                let x = left_edge + button_width / 2.0 - size / 2.0;

                // Resize the PNG image to match the specified size
                let resized_png = resize(
                    png,
                    size as u32,
                    size as u32,
                    FilterType::Lanczos3,
                );

                // Convert the resized PNG image to a Cairo ImageSurface
                let png_surface = ImageSurface::create(
                    Format::ARgb32,
                    size as i32,
                    size as i32,
                ).expect("Failed to create PNG surface");

                let png_context = Context::new(&png_surface)
                    .expect("Failed to create PNG context");

                // Iterate over the pixels of the resized PNG image and paint them on the Cairo surface
                for (x_pixel, y_pixel, pixel) in resized_png.enumerate_pixels() {
                    let channels = pixel.channels();
                    let (r, g, b, a) = (channels[0], channels[1], channels[2], channels[3]);
                    let _ = png_context.set_source_rgba(
                        r as f64 / 255.0,
                        g as f64 / 255.0,
                        b as f64 / 255.0,
                        a as f64 / 255.0,
                    );
                    let _ = png_context.rectangle(
                        x_pixel as f64,
                        y_pixel as f64,
                        1.0,
                        1.0,
                    );
                    let _ = png_context.fill();
                }

                // Composite the PNG surface onto the main context (the `c` context)
                let _ = c.set_source_surface(&png_surface, x, y);
                let _ = c.paint().expect("Failed to composite PNG image");
            },
            ButtonImage::Time(use_24_hour) => {
                let current_time = Local::now();
            
                // Format the time as a string
                let day_of_month = current_time.format("%e").to_string();
                let day_of_month = day_of_month.trim_start(); // Remove leading space if present
                let twelve_hour = current_time.format("%l").to_string();
                let twelve_hour = twelve_hour.trim_start(); // Remove leading space if present
                let formatted_time; 
                match use_24_hour {
                    0 => {
                        formatted_time = format!(
                        "{}:{} {}    {} {} {}",
                        twelve_hour,
                        current_time.format("%M"),
                        current_time.format("%p"),
                        current_time.format("%a"),
                        day_of_month,
                        current_time.format("%b")
                        );
                    }
                    1 => {
                        formatted_time = format!(
                        "{}:{}    {} {} {}",
                        current_time.format("%H"),
                        current_time.format("%M"),
                        current_time.format("%a"),
                        day_of_month,
                        current_time.format("%b")
                        );
                    }
                    _ => {
                        formatted_time = "".to_string();
                    }
                }
                // Calculate the text extents for the formatted time
                let time_extents = c.text_extents(&formatted_time).unwrap();

                // Display the formatted time
                c.move_to(
                    left_edge + button_width / 2.0 - time_extents.width() / 2.0,
                    height / 2.0 + time_extents.height() / 2.0,
                );
                c.show_text(&formatted_time).unwrap();
            },
            _ => {
            }
        }
    }
    fn set_active<F>(&mut self, uinput: &mut UInputHandle<F>, active: bool)
    where
        F: AsRawFd,
    {
        if self.active != active {
            self.active = active;
            self.changed = true;

            toggle_key(uinput, self.action, active as i32);
        }
    }
}

struct FunctionLayer {
    buttons: Vec<Button>,
}

impl FunctionLayer {
    fn draw(
        &mut self,
        surface: &ImageSurface,
        config: &Config,
        complete_redraw: bool,
    ) -> Vec<ClipRect> {
        let c = Context::new(&surface).unwrap();
        let mut modified_regions = Vec::new();
        let height = surface.width();
        let width = surface.height();
        c.translate(height as f64, 0.0);
        c.rotate((90.0f64).to_radians());
        let button_width = width as f64 / (self.buttons.len() + 1) as f64;
        let spacing_width = (width as f64 - self.buttons.len() as f64 * button_width)
            / (self.buttons.len() - 1) as f64;
        let radius = 8.0f64;
        let bot = (height as f64) * 0.15;
        let top = (height as f64) * 0.85;
        if complete_redraw {
            c.set_source_rgb(0.0, 0.0, 0.0);
            c.paint().unwrap();
        }
        c.select_font_face(&config.ui.font, FontSlant::Normal, FontWeight::Normal);
        c.set_font_size(32.0);
        for (i, button) in self.buttons.iter_mut().enumerate() {
            if !button.changed && !complete_redraw {
                continue;
            };

            let left_edge = i as f64 * (button_width + spacing_width);
            if !complete_redraw {
                c.set_source_rgb(0.0, 0.0, 0.0);
                if button.action == Key::Time {
                    c.rectangle(
                        left_edge,
                        bot - radius,
                        button_width * 3.0,
                        top - bot + radius * 2.0,
                    );
                } else {
                    c.rectangle(
                        left_edge,
                        bot - radius,
                        button_width,
                        top - bot + radius * 2.0,
                    );
                }
                c.fill().unwrap();
            }
            let color = if button.active { 
                BUTTON_COLOR_ACTIVE
            } else { 
                BUTTON_COLOR_INACTIVE
            };

            if (button.action != Key::Time &&
               button.action != Key::Unknown &&
               button.action != Key::Macro1 &&
               button.action != Key::Macro2 &&
               button.action != Key::Macro3 &&
               button.action != Key::Macro4) &&
               ((button.action != Key::WWW &&
                button.action != Key::AllApplications &&
                button.action != Key::Calc &&
                button.action != Key::File &&
                button.action != Key::Prog1 &&
                button.action != Key::Prog2 &&
                button.action != Key::Prog3 &&
                button.action != Key::Prog4) ||
                button.active) {
                // draw box with rounded corners
                c.set_source_rgb(color, color, color);
                c.new_sub_path();
                let left = left_edge + radius;
                let right = left_edge + button_width - radius;
                c.arc(
                    right,
                    bot,
                    radius,
                    (-90.0f64).to_radians(),
                    (0.0f64).to_radians(),
                );
                c.arc(
                    right,
                    top,
                    radius,
                    (0.0f64).to_radians(),
                    (90.0f64).to_radians(),
                );
                c.arc(
                    left,
                    top,
                    radius,
                    (90.0f64).to_radians(),
                    (180.0f64).to_radians(),
                );
                c.arc(
                    left,
                    bot,
                    radius,
                    (180.0f64).to_radians(),
                    (270.0f64).to_radians(),
                );
                c.close_path();
                c.fill().unwrap();
            }
            c.set_source_rgb(1.0, 1.0, 1.0);
            if button.action == Key::Time {
                button.render(&c, height as f64, left_edge, button_width * 3.0);
            } else {
                button.render(&c, height as f64, left_edge, button_width);
            }

            button.changed = false;
            if button.action == Key::Time {
            modified_regions.push(ClipRect {
                x1: height as u16 - top as u16 - radius as u16,
                y1: left_edge as u16,
                x2: height as u16 - bot as u16 + radius as u16,
                y2: left_edge as u16 + button_width as u16 * 3,
            });
            } else {
            modified_regions.push(ClipRect {
                x1: height as u16 - top as u16 - radius as u16,
                y1: left_edge as u16,
                x2: height as u16 - bot as u16 + radius as u16,
                y2: left_edge as u16 + button_width as u16,
            });
            }
        }

        if complete_redraw {
            vec![ClipRect {
                x1: 0,
                y1: 0,
                x2: height as u16,
                y2: width as u16,
            }]
        } else {
            modified_regions
        }
    }
}

struct Interface;

impl LibinputInterface for Interface {
    fn open_restricted(&mut self, path: &Path, flags: i32) -> Result<OwnedFd, i32> {
        let mode = flags & O_ACCMODE;

        OpenOptions::new()
            .custom_flags(flags)
            .read(mode == O_RDONLY || mode == O_RDWR)
            .write(mode == O_WRONLY || mode == O_RDWR)
            .open(path)
            .map(|file| file.into())
            .map_err(|err| err.raw_os_error().unwrap())
    }
    fn close_restricted(&mut self, fd: OwnedFd) {
        _ = File::from(fd);
    }
}

fn button_hit(num: u32, idx: u32, width: u16, height: u16, x: f64, y: f64) -> bool {
    let button_width = width as f64 / (num + 1) as f64;
    let spacing_width = (width as f64 - num as f64 * button_width) / (num - 1) as f64;
    let left_edge = idx as f64 * (button_width + spacing_width);
    if x < left_edge || x > (left_edge + button_width) {
        return false;
    }
    y > 0.09 * height as f64 && y < 0.91 * height as f64
}

fn emit<F>(uinput: &mut UInputHandle<F>, ty: EventKind, code: u16, value: i32)
where
    F: AsRawFd,
{
    uinput
        .write(&[input_event {
            value: value,
            type_: ty as u16,
            code: code,
            time: timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
        }])
        .unwrap();
}

fn toggle_key<F>(uinput: &mut UInputHandle<F>, code: Key, value: i32)
where
    F: AsRawFd,
{
    emit(uinput, EventKind::Key, code as u16, value);
    emit(
        uinput,
        EventKind::Synchronize,
        SynchronizeKind::Report as u16,
        0,
    );
}

#[derive(Deserialize)]
struct ButtonConfig {
    #[serde(default)]
    label: String,
    #[serde(default)]
    key: String,
    mode: String,
    #[serde(default)]
    theme: String,
}

#[derive(Deserialize)]
struct LayerButtonsConfig {
    buttons: Vec<ButtonConfig>,
}

#[derive(Deserialize)]
struct LayersConfig {
    primary_layer_buttons: LayerButtonsConfig,
    secondary_layer_buttons: LayerButtonsConfig,
    tertiary_layer_buttons: LayerButtonsConfig,
    tertiary2_layer_buttons: LayerButtonsConfig,
    tertiary3_layer_buttons: LayerButtonsConfig,
}

#[repr(usize)]
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum LayerType {
    Function,
    Special,
    SpecialExtended,
}

#[derive(Deserialize)]
struct UiConfig {
    primary_layer: LayerType,
    secondary_layer: LayerType,
    font: String,
    media_icon_theme: String,
    app_icon_theme: String,
}


#[derive(Deserialize)]
struct TimeConfig {
    use_24_hr: u16,
}

#[derive(Deserialize)]
struct Config {
    ui: UiConfig,
    time: TimeConfig,
    layers: LayersConfig,
}

impl Config {
    fn from_file(path: &str) -> Result<Self> {
        toml::from_str(&read_to_string(path)?).map_err(anyhow::Error::from)
    }
}

#[allow(dead_code)]
fn get_file_modified_time(path: &str) -> Option<SystemTime> {
    fs::metadata(path)
        .ok()
        .map(|metadata| metadata.modified().ok())
        .flatten()
}





lazy_static! {
    static ref KEY_MAP: HashMap<String, Key> = {
        let mut map = HashMap::new();
        for key in Key::iter() {
            map.insert(format!("Key::{:?}", key), key);
        }
        map
    };
}

fn build_layer_vectors(buttons: &Vec<ButtonConfig>, config: &Config) -> Vec<Button> {
    // helper to poputate layers with the given config
    let mut vector = Vec::new();
    for button_config in buttons {
        let label = button_config.label.as_str();
        let key = button_config.key.as_str();
        let theme = button_config.theme.as_str();
        let mode = button_config.mode.as_str();
        match mode {
            "blank" => vector.push(Button::new_blank()),
            "time" => vector.push(Button::new_time(config.time.use_24_hr)),
            "app" | "media" | "text" => {
                // handle missing input_linux::Keys
                let key_map = KEY_MAP.get(key);
                if key_map == None {
                    eprintln!("Could not find input_linux::Key {} for button {}. Ignored!", key, label);
                    continue;
                }
                if mode == "app" {
                    // if theme is an empty string assign the global theme
                    let theme = theme.is_empty().then(|| config.ui.app_icon_theme.as_str()).unwrap_or(theme);
                    vector.push(Button::new_icon(label, *key_map.unwrap(), theme));
                }
                else if mode == "media" {
                    // if theme is an empty string assign the global theme
                    let theme = theme.is_empty().then(|| config.ui.media_icon_theme.as_str()).unwrap_or(theme);
                    vector.push(Button::new_icon(label, *key_map.unwrap(), theme));
                } else {vector.push(Button::new_text(label, *key_map.unwrap()))};
            },
            _ => eprintln!("Could not find mode {} for button {}!", mode, label),
        }
    }
    vector
}



fn initialize_layers(config: &Config) -> [FunctionLayer; 5] {
    let primary_layer = FunctionLayer {
        buttons: build_layer_vectors(&config.layers.primary_layer_buttons.buttons, &config),
    };

    let secondary_layer = FunctionLayer {
        buttons: build_layer_vectors(&config.layers.secondary_layer_buttons.buttons, &config),
    };

    let tertiary_layer = FunctionLayer {
        buttons: build_layer_vectors(&config.layers.tertiary_layer_buttons.buttons, &config),
    };

    let tertiary2_layer = FunctionLayer {
        buttons: build_layer_vectors(&config.layers.tertiary2_layer_buttons.buttons, &config),
    };

    let tertiary3_layer = FunctionLayer {
        buttons: build_layer_vectors(&config.layers.tertiary3_layer_buttons.buttons, &config),
    };

    [primary_layer, secondary_layer, tertiary_layer, tertiary2_layer, tertiary3_layer]
}

fn main() {
    let mut config = Config::from_file(CONFIG_PATH).unwrap();
    let mut last_modified_time = get_file_modified_time(CONFIG_PATH);
    let mut uinput = UInputHandle::new(OpenOptions::new().write(true).open("/dev/uinput").unwrap());
    let mut backlight = BacklightManager::new();

    // drop privileges to input and video group
    let groups = ["input", "video"];

    PrivDrop::default()
        .user("nobody")
        .group("nobody")
        .group_list(&groups)
        .apply()
        .unwrap_or_else(|e| panic!("Failed to drop privileges: {}", e));

    let mut active_layer = config.ui.primary_layer as usize;
    let mut layers = initialize_layers(&config);

    let mut needs_complete_redraw = true;
    let mut drm = DrmBackend::open_card().unwrap();
    let (height, width) = drm.mode().size();
    let fb_info = drm.fb_info().unwrap();
    let pitch = fb_info.pitch();
    let cpp = fb_info.bpp() / 8;

    if width >= 2170 {
        for layer in &mut layers {
            layer.buttons.insert(0, Button::new_text("esc", Key::Esc));
        }
    }

    let mut surface = ImageSurface::create(Format::ARgb32, height as i32, width as i32).unwrap();
    let mut input_tb = Libinput::new_with_udev(Interface);
    let mut input_main = Libinput::new_with_udev(Interface);
    input_tb.udev_assign_seat("seat-touchbar").unwrap();
    input_main.udev_assign_seat("seat0").unwrap();
    let pollfd_tb = PollFd::new(input_tb.as_raw_fd(), PollFlags::POLLIN);
    let pollfd_main = PollFd::new(input_main.as_raw_fd(), PollFlags::POLLIN);
    uinput.set_evbit(EventKind::Key).unwrap();
    for layer in &layers {
        for button in &layer.buttons {
            uinput.set_keybit(button.action).unwrap();
        }
    }
    let mut dev_name_c = [0 as c_char; 80];
    let dev_name = "Dynamic Function Row Virtual Input Device".as_bytes();
    for i in 0..dev_name.len() {
        dev_name_c[i] = dev_name[i] as c_char;
    }
    uinput
        .dev_setup(&uinput_setup {
            id: input_id {
                bustype: 0x19,
                vendor: 0x1209,
                product: 0x316E,
                version: 1,
            },
            ff_effects_max: 0,
            name: dev_name_c,
        })
        .unwrap();
    uinput.dev_create().unwrap();

    let mut digitizer: Option<InputDevice> = None;
    let mut touches = HashMap::new();
    loop {
        let current_modified_time = get_file_modified_time(CONFIG_PATH);
        if current_modified_time != last_modified_time {
            match Config::from_file(CONFIG_PATH) {
                Ok(new_config) => {
                    config = new_config;
                    last_modified_time = current_modified_time;
                    layers = initialize_layers(&config);
                    if width < 2170 {
                        for layer in &mut layers {
                        layer.buttons.insert(0, Button::new_text("esc", Key::Esc));
                        }
                    }
                    let refreshed_layer = config.ui.primary_layer as usize;
                    active_layer = refreshed_layer;
                    needs_complete_redraw = true;
                }
                Err(e) => {
                    eprintln!("Failed to reload configuration: {}", e);
                }
            }
        }
	for button in &mut layers[active_layer].buttons {
    	    if button.action == Key::Time {
                button.changed = true;
    	    }
        }
        if needs_complete_redraw || layers[active_layer].buttons.iter().any(|b| b.changed) {
            let clips = layers[active_layer].draw(&surface, &config, needs_complete_redraw);
            let data = surface.data().unwrap();
            let mut fb = drm.map().unwrap();

            for clip in &clips {
                let base_offset =
                    clip.y1 as usize * pitch as usize + clip.x1 as usize * cpp as usize;
                let len = (clip.x2 - clip.x1) as usize * cpp as usize;

                for i in 0..(clip.y2 - clip.y1) {
                    let offset = base_offset + i as usize * pitch as usize;
                    let range = offset..(offset + len);
                    fb.as_mut()[range.clone()].copy_from_slice(&data[range]);
                }
            }

            drop(fb);
            drm.dirty(&clips[..]).unwrap();
            needs_complete_redraw = false;
        }
        poll(&mut [pollfd_tb, pollfd_main], TIMEOUT_MS).unwrap();
        input_tb.dispatch().unwrap();
        input_main.dispatch().unwrap();
        for event in &mut input_tb.clone().chain(input_main.clone()) {
            backlight.process_event(&event);
            match event {
                Event::Device(DeviceEvent::Added(evt)) => {
                    let dev = evt.device();
                    if dev.name().contains(" Touch Bar") {
                        digitizer = Some(dev);
                    }
                }
                Event::Keyboard(KeyboardEvent::Key(key)) => {
                    if key.key() == Key::Fn as u32 {
                        let new_layer = match key.key_state() {
                            KeyState::Pressed => config.ui.secondary_layer as usize,
                            KeyState::Released => config.ui.primary_layer as usize,
                        };
                        if active_layer != new_layer {
                            active_layer = new_layer;
                            needs_complete_redraw = true;
                        }
                        } else if key.key() == Key::Macro1 as u32 && key.key_state() == KeyState::Pressed {
                            active_layer = 3;
                            needs_complete_redraw = true;
                        } else if key.key() == Key::Macro2 as u32 && key.key_state() == KeyState::Pressed {
                            active_layer = 2;
                            needs_complete_redraw = true;
                        } else if key.key() == Key::Macro3 as u32 && key.key_state() == KeyState::Pressed {
                            active_layer = 4;
                            needs_complete_redraw = true;
                    }
                }
                Event::Touch(te) => {
                    if Some(te.device()) != digitizer || backlight.current_bl() == 0 {
                        continue;
                    }
                    match te {
                        TouchEvent::Down(dn) => {
                            let x = dn.x_transformed(width as u32);
                            let y = dn.y_transformed(height as u32);
                            let btn = (x
                                / (width as f64 / layers[active_layer].buttons.len() as f64))
                                as u32;
                            if button_hit(
                                layers[active_layer].buttons.len() as u32,
                                btn,
                                width,
                                height,
                                x,
                                y,
                            ) {
                                let button = &mut layers[active_layer].buttons[btn as usize];
                                if button.action == Key::Unknown || button.action == Key::Time {
                                    continue;
                                }
                                touches.insert(dn.seat_slot(), (active_layer, btn));
                                layers[active_layer].buttons[btn as usize]
                                    .set_active(&mut uinput, true);
                            }
                        }
                        TouchEvent::Motion(mtn) => {
                            if !touches.contains_key(&mtn.seat_slot()) {
                                continue;
                            }

                            let x = mtn.x_transformed(width as u32);
                            let y = mtn.y_transformed(height as u32);
                            let (layer, btn) = *touches.get(&mtn.seat_slot()).unwrap();
                            let hit = button_hit(
                                layers[layer].buttons.len() as u32,
                                btn,
                                width,
                                height,
                                x,
                                y,
                            );
                                let button = &mut layers[layer].buttons[btn as usize];
                                if button.action == Key::Unknown || button.action == Key::Time {
                                    continue;
                                }
                            button.set_active(&mut uinput, hit);
                        }
                        TouchEvent::Up(up) => {
                            if !touches.contains_key(&up.seat_slot()) {
                                continue;
                            }
                            let (layer, btn) = *touches.get(&up.seat_slot()).unwrap();
                            let button = &mut layers[layer].buttons[btn as usize];
                            if button.action == Key::Unknown || button.action == Key::Time {
                                continue;
                            }
                            button.set_active(&mut uinput, false);
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        backlight.update_backlight();
    }
}
