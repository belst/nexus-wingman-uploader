# GW2 Arcdps Log Uploader

Program to upload logs to both [dps.report](https://dps.report/) and [gw2wingman](https://gw2wingman.nevermindcreations.de)

![Screenshot](https://i.imgur.com/ffX7dQQ.png)

![Settings](https://i.imgur.com/XBq5wh9.png)

## Requirements:

- [Arcdps](https://www.deltaconnected.com/arcdps/) (to create logs)
- [Nexus](https://raidcore.gg/Nexus) (to load the addon)

## Installation:

### Automatic
Install from `Wingman Uploader` from the Nexus Addon Library.

### Manual
Download `log-uploader.dll` from [Releases](https://github.com/belst/nexus-wingman-uploader/releases/)

Move the file in `<Gw2Directory>/addons`.

## Configuring

- Settings Location: `<Gw2Directory>addons/wingman-uploader/settings.json`.
- `logpath`: Location of the arcdps logs (Default: `%userprofile%/Documents/Guild Wars 2/addons/arcdps/arcdps.cbtlogs`)
- `dpsreport_token`: Change this if you want to specify a dps report session token (leave empty to use generated one)
- `show_window`: Wether the window should be shown on startup or not (Stores last window state)
- `enable_wingman`: Whether uploading to wingman should be enabled or not
- `enable_dpsreport`: Whether uploading to dpsreport should be enabled or not
- `filter_wingman`: List of ids which should be ignored when uploading to wingman
- `filter_dpsreport`: List of ids which should be ignored when uploading to dpsreport
