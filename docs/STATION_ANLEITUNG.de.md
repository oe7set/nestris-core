# Anleitung: Capture-Station (Debian) einrichten

Schritt für Schritt vom Windows-PC zur fertigen, selbststartenden Station.
Technische Referenz (MQTT-Topics, Payloads, Cheat-Erkennung, Validierung):
[STATION.md](STATION.md).

```
NES ─► USB-Capture-Karte ─► nestris-station ─► MQTT ─► Host-PC (Broker)
ESP32 RFID-Leser (USB) ──────►     │
                                   └─► /var/lib/nestris-station/recordings/*.ngf.gz
```

Pro Station läuft **ein** Dienst `nestris-station` (eine Capture-Karte, ein
RFID-Leser). Stationen werden über `station.id` unterschieden.

---

## 1. Paket bauen (einmalig, am Windows-PC)

Voraussetzung: Docker Desktop läuft.

```powershell
cd D:\Projekte\Retroverse\nestris-core
./tools/build-station-deb.ps1            # → dist\nestris-station_0.2.0-1_amd64.deb
```

Der Build läuft in einem Debian-12-Container und funktioniert daher auf
Debian 12 **und** 13 (amd64). Der erste Build dauert einige Minuten, danach
geht es schneller. MQTT über TLS: `./tools/build-station-deb.ps1 -Tls`
(im LAN normalerweise nicht nötig).

Ohne Docker geht es auch in WSL (Debian/Ubuntu):
`cargo install cargo-deb --locked && cargo deb -p nestris-station`.

## 2. Was auf die Station muss

**Nur die `.deb`-Datei.** Sie enthält:

| Datei | Zweck |
|---|---|
| `/usr/bin/nestris-station` | das Programm |
| `/usr/lib/systemd/system/nestris-station.service` | Dienst (Autostart, Neustart, Watchdog) |
| `/etc/nestris-station/station.toml` | Konfiguration (bleibt bei Updates erhalten) |
| `/etc/nestris-station/env` | Passwort / Overrides pro PC (bleibt erhalten) |
| `/usr/share/doc/nestris-station/` | Beispiel-Config, udev-Regeln |

`ffmpeg` wird von apt automatisch mitinstalliert. Der Benutzer `nestris`
(Gruppen `video`, `dialout`) wird angelegt.

```powershell
scp dist\nestris-station_*.deb user@station-1:/tmp/
```

## 3. Installieren (auf der Station)

```sh
sudo apt update
sudo apt install /tmp/nestris-station_*.deb
sudo apt install v4l-utils mosquitto-clients chrony   # Diagnose + Zeit-Sync
```

**Update später:** neue `.deb` genauso installieren. Die Konfiguration bleibt
erhalten, danach `sudo systemctl restart nestris-station`.

## 4. Geräte finden

```sh
nestris-station list-devices
ls -l /dev/v4l/by-id/ /dev/serial/by-id/
v4l2-ctl --list-formats-ext -d /dev/v4l/by-id/usb-MACROSILICON_USB_Video_20200909-video-index0
```

Immer die **by-id**-Pfade verwenden. `/dev/video0` und `/dev/ttyUSB0` können
nach einem Neustart vertauscht sein.

## 5. Konfigurieren

```sh
sudoedit /etc/nestris-station/station.toml
```

Das Minimum für eure Stationen (PAL-Karte, 720x576 @ 50 fps, wie im
go2rtc-Befehl):

```toml
[station]
id = "station-1"            # eindeutig pro PC: station-1, station-2, ...
name = "Station 1"

[capture]
device = "/dev/v4l/by-id/usb-MACROSILICON_USB_Video_20200909-video-index0"
input_format = "mjpeg"
width = 720
height = 576
fps = 50
scale_width = 720           # = Capture-Größe, sonst wird verzerrt hochskaliert
scale_height = 576

[rfid]
port = "/dev/serial/by-id/usb-Silicon_Labs_CP2102_..."   # aus Schritt 4

[mqtt]
host = "192.168.1.10"       # IP des Host-PCs mit dem MQTT-Broker
port = 1883
username = ""               # falls der Broker Login verlangt
```

Alle anderen Werte haben sinnvolle Defaults (siehe Kommentare in der Datei).
Unbekannte Schlüssel ergeben einen Fehler, Tippfehler fallen also sofort auf.

**Tipp für mehrere Stationen:** Auf allen PCs dieselbe `station.toml` verwenden
und nur in `/etc/nestris-station/env` pro PC die ID setzen:

```sh
sudoedit /etc/nestris-station/env
```
```sh
NESTRIS_STATION__STATION__ID=station-2
NESTRIS_STATION__STATION__NAME="Station 2"
NESTRIS_STATION__MQTT__PASSWORD=geheim     # Passwort nie in station.toml
```

Das Schema ist `NESTRIS_STATION__<SEKTION>__<SCHLÜSSEL>`. Das gilt für jeden
Wert, auch für `NESTRIS_STATION__MQTT__HOST`.

### Konfiguration aus NestrisLTM (ab Station 0.3.0)

Sobald die Station mit dem Broker verbunden ist, lassen sich fast alle
Werte bequem in NestrisLTM ändern: *Stationen & Geräte → Konfiguration*.
Dort gibt es eine **Vorlage für alle Stationen** und pro Station eigene
Werte. Nach dem Speichern übernimmt die Station die Werte und startet ihre
Erkennung neu (läuft gerade ein Spiel, erst danach). Die Werte liegen auf
der Station in `/var/lib/nestris-station/remote.json`.

Lokal in `station.toml` bzw. `env` bleiben dann nur noch: `station.id`,
`mqtt.host`, Passwörter/Token und eventuell `rfid.port`. Alles, was in der
`env`-Datei oder per `--set` steht, gewinnt immer und erscheint in NestrisLTM
als „gesperrt“ (z. B. `station.name`, wenn es in der env-Datei steht).
Nie aus der Ferne änderbar sind Station-ID, Broker, Host-URL/Token, Pfade und
Updates. Eine falsche Einstellung kann die Station also nicht vom Host
abschneiden.

Leistung prüfen: *Stationen & Geräte → Leistung* zeigt pro Station Kamera-
und Erkennungs-FPS, verworfene Bilder, Engine-Zeit, CPU und wie viele
Live-Nachrichten bei NestrisLTM ankommen. Direkt auf der Station misst
`nestris-station bench` dasselbe ohne Broker (siehe `docs/DOWNSCALE.md`).

### Wie findet die Station den Broker?

Nur über `mqtt.host` (IP oder DNS-Name). Die Station sucht nicht selbst im
Netz. Deshalb:

- Dem Host-PC eine **feste IP** geben (oder eine DHCP-Reservierung im Router).
- Alternativ einen Namen, den alle Stationen auflösen können (Router-DNS,
  oder `/etc/hosts` auf jeder Station: `192.168.1.10  retroverse-host`,
  dann `host = "retroverse-host"`).

## 6. Prüfen und starten

```sh
# Als Dienst-Benutzer und mit der env-Datei, genau wie systemd es später macht:
sudo -u nestris sh -c 'set -a; . /etc/nestris-station/env; nestris-station check-config'  # fertige Config, Passwort maskiert
sudo -u nestris sh -c 'set -a; . /etc/nestris-station/env; nestris-station test-mqtt'     # erreicht die Station den Broker?

sudo systemctl start nestris-station
journalctl -u nestris-station -f                                                          # Live-Log
```

(Ein nacktes `nestris-station check-config` liest die env-Datei **nicht**,
und `station.toml` ist nur für root und die Gruppe `nestris` lesbar.)

Am Host-PC sieht man alles, was die Stationen senden:

```sh
mosquitto_sub -h 192.168.1.10 -v -t 'retroverse/nestris/#'
```

Innerhalb weniger Sekunden sollte `retroverse/nestris/station-1/status` mit
`"capture":"ok","capture_detail":"720x576"` kommen. Wird eine Karte auf den
Leser gelegt, kommt `.../player`.

## 7. Autostart und Selbstheilung

Der Dienst ist nach der Installation schon **aktiviert** und startet nach
jedem Boot von selbst. Es ist nichts weiter zu tun.

| Problem | Was passiert |
|---|---|
| Programm stürzt ab | systemd startet es nach 3 s neu, unbegrenzt oft |
| Programm hängt | Watchdog (30 s) beendet es und startet es neu |
| Erkennung stürzt bei einem Frame ab | Engine wird im Prozess neu aufgebaut, das Spiel wird markiert |
| ffmpeg bricht ab / Karte liefert keine Bilder | Neustart der Capture nach 5 s, Backoff bis 30 s |
| Capture-Karte abgesteckt | Status `waiting_for_device`, läuft weiter, sobald sie wieder steckt |
| RFID-Leser abgesteckt | Reconnect-Schleife, Status `rfid: offline` |
| Leser mit alter Firmware | Status `rfid: outdated`: Firmware `nestris-rfid-reader` flashen (siehe dort `docs/FLASHING.md`) |
| Netzwerk / Broker weg | Reconnect-Schleife; Spielergebnisse warten im Spool auf der Platte und werden nachgesendet |
| Stromausfall / Neustart | Dienst startet beim Boot, der Spool wird nachgesendet |

## 8. go2rtc: zwei Betriebsarten

Eine USB-Capture-Karte kann **nur von einem Programm gleichzeitig** geöffnet
werden. Entweder liest nestris-station die Karte direkt, oder go2rtc hält die
Karte und nestris-station liest den Stream von go2rtc. Umschalten geht mit
einer Zeile in `station.toml`.

### A) Turnierbetrieb (empfohlen): direkt

```toml
[capture]
device = "/dev/v4l/by-id/usb-MACROSILICON_USB_Video_20200909-video-index0"
```

go2rtc muss dann **aus** sein, sonst belegt es die Karte, sobald jemand den
Stream öffnet:

```sh
sudo systemctl disable --now go2rtc    # bzw. wie euer go2rtc-Dienst heißt
```

Vorteile: am wenigsten Latenz und Teile, und das Warten auf eine abgesteckte
Karte funktioniert.

### B) Debug: über go2rtc (mit Live-Vorschau im Browser)

go2rtc läuft mit eurem bestehenden Source (in `go2rtc.yaml`, Name z. B. `nes`):

```yaml
streams:
  nes: "exec:ffmpeg -hide_banner -loglevel error -f v4l2 -input_format mjpeg -video_size 720x576 -framerate 50 -i /dev/v4l/by-id/usb-MACROSILICON_USB_Video_20200909-video-index0 -c:v copy -an -f mjpeg -"
```

Das passt so. `-c:v copy` reicht die JPEGs der Karte unverändert durch, es
kostet also keine Qualität und kaum CPU.

In `station.toml`:

```toml
[capture]
device = "http://127.0.0.1:1984/api/stream.mjpeg?src=nes"
# width/height/fps/input_format werden bei Streams ignoriert,
# scale_width/scale_height (720x576) gelten weiter.
```

```sh
sudo systemctl restart nestris-station
```

Jetzt ist nestris-station ein Dauer-Zuschauer von go2rtc. Parallel kann man
im Browser unter `http://station-1:1984/` zusehen. Fällt go2rtc aus, verbindet
sich die Station automatisch neu (Status `reconnecting`).

- HTTP-MJPEG statt RTSP verwenden: go2rtc gibt die JPEGs 1:1 weiter.
  `rtsp://127.0.0.1:8554/nes` geht auch (die Station nutzt RTSP über TCP),
  packt MJPEG aber in RTP um.
- Das Warten auf die abgesteckte Karte übernimmt in diesem Modus go2rtc. Die
  Station sieht nur „Stream weg“ und verbindet sich mit Backoff neu.

## 9. Host-PC: MQTT-Broker

Auf dem Host-PC muss ein Broker laufen, z. B. Mosquitto
(Windows-Installer von mosquitto.org, oder Docker). Minimale
`mosquitto.conf`:

```
listener 1883 0.0.0.0
allow_anonymous false
password_file C:\mosquitto\passwd      # mosquitto_passwd -c C:\mosquitto\passwd station
persistence true
```

Nur im abgeschotteten Turnier-LAN ist `allow_anonymous true` vertretbar.
Dann in der Windows-Firewall eingehend **TCP 1883** freigeben.

Für die Host-Software, die die Ergebnisse auswertet:

- Die Ergebnisse `retroverse/nestris/<station>/event/game_end` kommen
  **mindestens einmal**, nach einem Netzwerkausfall also eventuell doppelt.
  Deshalb per `game_id` deduplizieren.
- `valid: false` heißt: Das Ergebnis vor der Wertung prüfen. `cheated > 0`
  heißt: Der Select-Trick wurde verwendet.
- `status` ist retained, mit Last Will `offline`: Man sieht sofort, welche
  Station tot ist.
- `live` enthält neben Score, Lines, Level, Next und Statistik auch das
  **Spielfeld** (`playfield`): 20 Zeilen à 10 Ziffern von oben nach unten.
  `0` ist leer, `1` weiß, `2`/`3` sind die beiden Farben des aktuellen
  Levels, die Farbe selbst ergibt sich aus `level`. Das fallende Stück ist
  enthalten. Außerhalb des Spiels ist das Feld `null`, auch während der
  Pause: Die Konsole versteckt das Brett in der Pause, also tut es die
  Zuschaueransicht auch. Gesendet wird bei jeder Änderung, höchstens
  `mqtt.live_max_hz`-mal pro Sekunde (Standard 60, also jeder NES-Frame, etwa
  18 KB/s pro Station).
  Mit `live_playfield = false` wird das Spielfeld abgeschaltet.
- Der Host-Konsument ist die neue Host-App `nestris-ltm` (NestrisLTM). Sie
  speichert Ergebnisse und Live-Frames in PostgreSQL.

## 10. Aufnahmen an NestrisLTM hochladen

Damit der Host jedes Spiel vollständig hat (Replay, Streitfälle), lädt die
Station jede gespeicherte Aufnahme nach Spielende hoch:

1. In NestrisLTM unter *Einstellungen → API-Tokens* einen Token mit dem Recht
   `stations` erzeugen (er wird nur einmal angezeigt).
2. Auf der Station den Token ablegen:
   `echo -n 'nltm_...' | sudo tee /etc/nestris-station/host-token && sudo chmod 640 /etc/nestris-station/host-token && sudo chown root:nestris /etc/nestris-station/host-token`
3. In `station.toml`:
   ```toml
   [host]
   url = "http://<host-ip>:7990"
   token_file = "/etc/nestris-station/host-token"
   ```
4. `sudo systemctl restart nestris-station`. Im Log steht nach jedem Spiel
   `recording uploaded`. Ist der Host nicht erreichbar, warten die Aufträge in
   `/var/lib/nestris-station/uploads` und gehen später raus.

## 11. Was man noch bedenken sollte

**Vor dem Event**

- [ ] Jede Station einmal komplett durchtesten: Konsole an, Karte auflegen,
      ein Spiel spielen. Am Host `mosquitto_sub` beobachten und `game_end`
      mit `valid: true` prüfen.
- [ ] Ausfälle simulieren: Capture-Karte ab- und anstecken, Netzwerkkabel
      ziehen (Spiel beenden, Kabel wieder rein → Ergebnis kommt nach),
      `sudo reboot` → Dienst läuft wieder.
- [ ] Hostname = Station-ID (`sudo hostnamectl set-hostname station-1`).
      Das macht Logs und SSH eindeutig.
- [ ] Zeit synchron (`chrony` installiert, `timedatectl` zeigt
      `System clock synchronized: yes`). Alle Zeitstempel sind UTC-Wanduhr.

**Hardware / System**

- [ ] BIOS: „Restore on AC Power Loss“ = **Power On**. Nach einem Stromausfall
      startet der PC dann von selbst.
- [ ] Kein Energiesparen:
      `sudo systemctl mask sleep.target suspend.target hibernate.target hybrid-sleep.target`
- [ ] USB-Autosuspend für Karte und Leser aus: die udev-Regeln aus
      `/usr/share/doc/nestris-station/99-nestris-station.rules` nach
      `/etc/udev/rules.d/` kopieren. Die IDs `534d:2109` passen zur
      MacroSilicon-Karte; den RFID-Chip mit `lsusb` prüfen, CP2102 = `10c4:ea60`,
      CH340 = `1a86:7523`. Danach
      `sudo udevadm control --reload-rules && sudo udevadm trigger`.
- [ ] Capture-Karte direkt am PC einstecken, nicht über einen passiven Hub.
- [ ] Während des Events keine automatischen Updates:
      `sudo systemctl disable --now unattended-upgrades apt-daily.timer apt-daily-upgrade.timer`
      (danach wieder aktivieren).
- [ ] Plattenplatz: Aufzeichnungen (`.ngf.gz`, klein) werden nach 30 Tagen
      oder ab 20 GB aufgeräumt (`[recording] keep_days`, `max_gb`).

**Betrieb**

```sh
systemctl status nestris-station
journalctl -u nestris-station --since today | grep -E 'WARN|ERROR'
ls /var/lib/nestris-station/spool         # sollte leer sein (= alles zugestellt)
ls /var/lib/nestris-station/recordings    # ein .ngf.gz pro Spiel (Beweis bei Streitfällen)
```

Mehr Log für eine Komponente: in `/etc/nestris-station/env`
`RUST_LOG=info,nestris_station::rfid=debug` setzen, dann den Dienst neu starten.

**Bildschirm-Erkennung:** Die Station erkennt Titel (auch mit dem
Retroverse-Logo des Event-ROMs), Spielmodus-Auswahl, Level-Auswahl, Spiel,
Pause, Game-Over-Vorhang, Raketen-Ende und Highscore-Eingabe am festen
NES-Kachelraster. Eine Pause beendet nie ein Spiel, egal wie lang sie dauert.
Einschalt- und Copyright-Bildschirm sowie Flashcart-Menüs gelten nicht als
Menü. Getestet ist das mit echten Aufnahmen einer Station; die Ergebnisse
stehen in `docs/VERIFICATION.md`. Mit einem anderen ROM-Hack oder einer
anderen Karte sollte man vor dem Event einmal aufnehmen und am Windows-PC
mit dem `nestris`-Werkzeug aus diesem Repository prüfen:

```sh
nestris screens eval --input aufnahme.mkv --timeline 25   # Zeitleiste + erkannte Spielstarts
```

**Aufnahmen:** Wer zum Testen mitschneidet, sollte auf die Bildrate
achten. Die bisherigen `aufnahme_*.mkv` sind mit 25 fps gespeichert, obwohl
die Karte 50 fps liefert; sie laufen dadurch halb so schnell. Reparieren
ohne Neukodierung: `ffmpeg -r 50 -i aufnahme.mkv -c copy aufnahme_50fps.mkv`.
Alternativ beim Auswerten `--fps 50` angeben.

**PAL:** Eine PAL-Konsole ist kein Problem. Spielfeld und Ziffern liegen an
derselben Stelle wie bei NTSC, und die Punkteberechnung (inkl.
Cheat-Erkennung) ist gleich. Wichtig ist nur, dass die Capture-Einstellungen
zur Karte passen (720x576, 50 fps).
