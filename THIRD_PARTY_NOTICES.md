# Third-party notices

Herdr Night Watch is released under the MIT License. The weather feature is a
separate runtime integration: it calls Open-Meteo over HTTPS and does not bundle
Open-Meteo source code or a copy of its service data.

## Open-Meteo weather and geocoding

The live window uses the following Open-Meteo services:

- [Forecast API](https://open-meteo.com/en/docs) for the current temperature and
  daily moon phase.
- [Geocoding API](https://open-meteo.com/en/docs/geocoding-api) for searching a
  city or postal code.

Please see the official [Open-Meteo terms](https://open-meteo.com/en/terms),
[license information](https://open-meteo.com/en/license), and the
[geocoding API license](https://github.com/open-meteo/geocoding-api#data-license)
before deploying this feature.

The free Open-Meteo endpoints are intended for open-source and non-commercial
use and are subject to fair-use limits. The Forecast API's temperature and
`moon_phase` response fields are covered by [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).
The Geocoding API's location data are covered by
[CC BY-NC 4.0](https://creativecommons.org/licenses/by-nc/4.0/).
Attribution, a link to the applicable license, and an indication of changes
are required. The geocoding project specifically asks for a link next to
displayed location data.

Herdr Night Watch shows an Open-Meteo attribution in the weather-location
dialog and links to the official service documentation here. The application
does not use weather, location, or moon-phase data for the safety-critical
watcher. The moon phase is not copied from a separate astronomy database; it
is the `daily=moon_phase` field returned by the Forecast API.

The MIT license of this repository does not grant permission to use the free
Open-Meteo service commercially. A commercial deployment must use an
appropriate Open-Meteo commercial/customer plan or replace the provider with a
service whose terms cover that use. The core watcher and the rest of this
repository remain MIT-licensed.

## Russo One font

The analog clock's cardinal numerals embed Russo One Regular from the official
[Google Fonts repository](https://github.com/google/fonts/tree/main/ofl/russoone).
The font file is bundled as `assets/fonts/RussoOne-Regular.ttf`; its complete
SIL Open Font License 1.1 is included alongside it as
`assets/fonts/RussoOne-OFL.txt`.
