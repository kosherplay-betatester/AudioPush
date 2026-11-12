# Code signing policy

Free code signing provided by [SignPath.io](https://signpath.io), certificate
by [SignPath Foundation](https://signpath.org).

## Committers and reviewers

Members of the `kosherplay-betatester` GitHub account commit and review
changes to this repository. Access requires multi-factor authentication.

## Privacy

AudioPush streams audio between machines on your own local network. It does
not transmit any information to the authors or to any third party, and it
contacts no server of its own. The only network traffic it creates is the
discovery, audio and microphone traffic described in the README, all of it
addressed to machines on your network that you chose.

It stores its settings in `%APPDATA%\AudioPush\settings.txt` on Windows, and
in the app's private storage on Android. That file holds the receivers you
have connected to and the pairing tokens you agreed to, and nothing else.
