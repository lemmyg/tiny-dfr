# tiny-dfr
The most basic dynamic function row daemon possible

# Config setup

/etc/tiny-dfr.conf

## Layouts

ui.primary_layer and ui.secondary_layer options:

* function: Classic F1-12 buttons layout.
* special: Apple classic media buttons layout.
* specialExtended: Apps and media buttons layout.

When `fn key` is pressed the `ui.secondary_layer` is activated.

Default layouts:

* layers.primary_layer_buttons: Classic F1-12 buttons layout. Used when 'function' layer is assigned.
* layers.secondary_layer_buttons: Apple classic media buttons layout. Used when 'special' layer is assigned.
* layers.tertiary_layer_buttons: Apps and media buttons layout. Used when 'specialExtended' layer is assigned.
* layers.tertiary2_layer_buttons: Extra apps sub layout. Used when 'specialExtended' layer is assigned.
* layers.tertiary3_layer_buttons: Apple media sub layout. Used when 'specialExtended' layer is assigned.

## Icon Themes

Default search paths: /usr/share/tiny-dfr/icons /usr/share/icons

* ui.media_icon_theme: icons theme name for buttons with media mode.
* ui.app_icon_theme: icons theme name for buttons with app mode.

## Time

* time.use_24_hr: Enable 24h time format 1 or 0. Default 1.


## Dependencies
pango, libinput, uinput enabled in kernel config

## License

tiny-dfr is licensed under the MIT license, as included in the [LICENSE](LICENSE) file.

* Copyright The Asahi Linux Contributors

Please see the Git history for authorship information.

tiny-dfr embeds Google's [material-design-icons](https://github.com/google/material-design-icons)
which are licensed under [Apache License Version 2.0](LICENSE.material)
