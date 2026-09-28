# Phương án chạy Brickwave trên NextUI (TrimUI Brick Pro)

## Kết luận

Khả năng triển khai là **cao**. Brickwave không cần đổi UI, backend SoundCloud,
playback engine hay bộ điều khiển vật lý. Phương án đúng là giữ binary ARM64
hiện tại và đóng gói nó thành một NextUI **Tool Pak** riêng.

Gói NextUI cục bộ `NextUI-20260719-0-all` là bản chính thức từ
`LoveRetro/NextUI`, tag `v6.14.0`, commit
`73e160c1a975e5f6db72e72d8b62453f13277753`. Base launcher của bản này nhận
TrimUI Brick Pro là:

- `PLATFORM=tg5040`
- `DEVICE=brickpro`
- `SDCARD_PATH=/mnt/SDCARD`
- `SYSTEM_PATH=/mnt/SDCARD/.system/tg5040`
- `USERDATA_PATH=/mnt/SDCARD/.userdata/tg5040`
- `SHARED_USERDATA_PATH=/mnt/SDCARD/.userdata/shared`
- `LOGS_PATH=/mnt/SDCARD/.userdata/tg5040/logs`

Tài liệu chính thức quy định Tool Pak nằm tại `Tools/tg5040/*.pak` và không
được cài vào `.system`, vì `.system` bị thay thế khi NextUI cập nhật:

- https://github.com/LoveRetro/NextUI
- https://github.com/LoveRetro/NextUI/blob/main/PAKS.md

## Những phần dùng nguyên

### Binary và đồ họa

Binary Brickwave 0.4.17 hiện tại là ARM64, dynamic loader
`/lib/ld-linux-aarch64.so.1`, GLIBC cao nhất `2.33`. Dependency trực tiếp chỉ
gồm:

- `libSDL2-2.0.so.0`
- `libm.so.6`
- `libpthread.so.0`
- `libc.so.6`
- `libdl.so.2`

NextUI không thay root filesystem StockOS. Base launcher của NextUI thêm cả
`/usr/trimui/bin` vào `PATH` và `/usr/trimui/lib` vào `LD_LIBRARY_PATH`.
NextUI thoát `nextui.elf` trước khi chạy `launch.sh` của Pak, nên Brickwave có
thể sở hữu SDL window/Mali renderer như trên StockOS. Giới hạn RGB565/OpenGL
trong `PAKS.md` nói về libretro qua `minarch`; nó không buộc một standalone
SDL2 Tool Pak như Brickwave phải dùng minarch.

### Playback

Giữ nguyên `/usr/trimui/bin/mplayer`, HLS prefetch, session audio cache, seek,
soft volume và toàn bộ backend đang hoạt động. NextUI vẫn để stock MPlayer và
stock libraries trong rootfs.

NextUI đặt `HOME=$USERDATA_PATH`. `audiomon.elf` ghi route Bluetooth/USB vào
`$USERDATA_PATH/.asoundrc`; MPlayer đọc đúng file này qua `HOME`. Launcher
Brickwave không được đổi `HOME`, nhờ vậy loa mặc định, Bluetooth A2DP và USB
audio có thể đi theo cấu hình NextUI.

### Input

NextUI vẫn chạy `trimui_inputd`. Các mapping Brickwave hiện tại có thể giữ:

- Analog: chuột ảo
- D-pad: điều hướng/cuộn
- A: click/xác nhận
- B: quay lại/hủy
- MENU/logo: mở xác nhận thoát Brickwave
- A trong hộp thoát: thoát
- B trong hộp thoát: hủy
- Y/Select: pause
- Start: resume, không tải lại bài
- L1/R1: previous/next

Log StockOS xác nhận MENU nằm ở `/dev/input/event1`, còn face buttons và D-pad
ở `/dev/input/event3`. Brickwave chỉ `EVIOCGRAB` thiết bị quảng bá `KEY_POWER`,
và nhả grab khi đóng. Trên NextUI cần test thêm phím volume vì `keymon.elf`
cũng đọc evdev. Nếu volume +/- nằm chung `event1`, grab cả thiết bị có thể chặn
volume trong lúc Brickwave chạy; khi đó sẽ thu hẹp cơ chế grab, không đổi
mapping ứng dụng.

## Hai điểm cần cô lập khỏi StockOS

### Quản lý màn hình

`trimui_host.rs` còn có `StockOsDisplaySleep`, đọc `shmvar` và ghi
`/tmp/system/set_brightness`. Tính năng sleep trong UI đang bị khóa và state
luôn bị ép về `false`, nên đường này hiện không tự tắt màn. Tuy nhiên gói NextUI
không nên gọi API độ sáng của StockOS.

Khi triển khai, thêm biến `BRICKWAVE_HOST=nextui`. Với host này:

- bỏ qua hoàn toàn truy vấn `shmvar brightness/dimtime`;
- không ghi `/tmp/system/set_brightness`;
- giữ screen sleep của Brickwave ở trạng thái disabled;
- để NextUI quản lý độ sáng trước và sau khi Pak thoát.

Deep sleep của NextUI được điều khiển trong tiến trình `nextui.elf`. Tiến trình
này không chạy trong lúc một standalone Pak đang mở, nên không tuyên bố
Brickwave hỗ trợ sleep/background audio của NextUI ở bản đầu tiên. MENU sẽ tiếp
tục là thao tác thoát an toàn của Brickwave.

### Artwork cache

Trên Linux, artwork cache hiện rơi về `/tmp/SoundCloudBrickPreview`, nên không
bền qua reboot. Thêm cấu hình `BRICKWAVE_ARTWORK_CACHE_DIR` và đặt nó vào
`.userdata`. Không thay đổi worker, allowlist, giới hạn ảnh hay texture cache.

## Cấu trúc gói đề xuất

```text
Brickwave-NextUI-00.4.17/
└── Tools/
    └── tg5040/
        ├── .media/
        │   └── Brickwave.png
        └── Brickwave.pak/
            ├── launch.sh
            ├── pak.json
            ├── README.md
            ├── LICENSE
            ├── THIRD_PARTY_NOTICES.md
            └── bin/
                └── brickwave
```

`Tools/tg5040/.media/Brickwave.png` là artwork riêng của mục Brickwave trong
danh sách Tools. Không thêm `Tools/tg5040/.media/bg.png`, vì file đó có thể đổi
background cho cả thư mục Tools.

Không mang `Apps/Brickwave/config.json` sang NextUI; đó là metadata của MainUI
StockOS và NextUI không đọc nó.

## Launcher NextUI

Launcher sẽ chạy foreground để main loop của NextUI chờ Brickwave kết thúc rồi
tự mở lại menu. Các nguyên tắc:

1. Chỉ nhận `PLATFORM=tg5040` và ưu tiên `DEVICE=brickpro`.
2. Giữ nguyên `HOME` do NextUI cung cấp.
3. Giữ `$SYSTEM_PATH/lib` ở đầu `LD_LIBRARY_PATH`, sau đó là
   `/usr/trimui/lib:/usr/lib:/lib`.
4. Đặt:
   - `SDL_VIDEODRIVER=mali`
   - `SDL_AUDIODRIVER=dummy`
   - `SOUNDCLOUD_MODE=live`
   - `BRICKWAVE_HOST=nextui`
5. Dữ liệu tài khoản và cài đặt:
   `$SHARED_USERDATA_PATH/Brickwave/data`.
6. Artwork cache:
   `$SHARED_USERDATA_PATH/Brickwave/artwork-cache`.
7. Runtime và audio cache phiên hiện tại:
   `$USERDATA_PATH/Brickwave/run`.
8. Log:
   `$LOGS_PATH/brickwave.log`.
9. Kiểm tra loader, binary và `/usr/trimui/bin/mplayer` trước khi chạy.
10. Trap `TERM/INT/HUP`, đợi child thoát và chỉ dọn lock/runtime thuộc
    Brickwave.

Không chạy `syncsettings.elf` trong bản đầu vì Brickwave không sửa hardware
brightness/volume khi khởi động. Chỉ bổ sung nếu test thiết bị chứng minh SDL
hoặc MPlayer làm thay đổi setting của NextUI.

## Lưu trạng thái đăng nhập

Không lưu session trong `Brickwave.pak`, vì Pak có thể bị ghi đè khi cập nhật.
`session.json` và `preferences.json` nằm trong
`.userdata/shared/Brickwave/data`.

Nếu cần giữ phiên đăng nhập từ bản StockOS trên cùng thẻ, thực hiện migration
một lần khi thư mục NextUI còn rỗng:

- nguồn: `/mnt/SDCARD/Apps/Brickwave/data/session.json`
- nguồn: `/mnt/SDCARD/Apps/Brickwave/data/preferences.json`
- đích: `/mnt/SDCARD/.userdata/shared/Brickwave/data/`

Migration chỉ copy hai file đã biết, không xóa hay sửa dữ liệu StockOS. Nếu
proof đã hết hạn, ứng dụng trở về QR login theo logic hiện tại.

## Các bước triển khai

### N1 — Portability guard

- Thêm `BRICKWAVE_HOST=nextui` để vô hiệu hóa API brightness StockOS.
- Thêm `BRICKWAVE_ARTWORK_CACHE_DIR`.
- Viết test cho việc chọn host và đường dẫn cache.
- Không đổi UI, backend, OAuth, player hay input mapping.

### N2 — Tool Pak

- Tạo launcher và `pak.json`.
- Dùng lại binary ARM64 đã build; rebuild chỉ khi N1 làm thay đổi source.
- Tạo icon menu từ logo Brickwave hiện có.
- Tạo ZIP cài thủ công với cây `Tools/tg5040/...`.
- Giữ nguyên gói StockOS để rollback.

### N3 — Kiểm tra tĩnh

- `cargo fmt --check`, `cargo check`, `cargo test`.
- Cross-build ARM64 với feature `trimui-sdl2`.
- Xác nhận ELF AArch64, interpreter và GLIBC tối đa 2.33.
- `sh -n launch.sh`.
- Kiểm tra ZIP không chứa `.system` và không chứa session/log/token.
- Kiểm tra quyền executable của `launch.sh` và `bin/brickwave` trong archive.

### N4 — Smoke test trên Brick Pro chạy NextUI

1. Brickwave xuất hiện trong Tools với đúng icon.
2. Mở được fullscreen 1024×768 và trở về đúng danh sách Tools sau khi thoát.
3. Analog, D-pad, A, B, MENU, Y, Start, L1, R1 hoạt động.
4. MENU mở hộp thoát; A thoát, B hủy; không nháy màn và không kẹt input.
5. Volume +/- của NextUI vẫn hoạt động trong lúc Brickwave mở.
6. Wi-Fi, QR login và restore session hoạt động.

### N5 — Playback và persistence

1. Phát bài đầu, bài khác, pause/resume, previous/next, seek và volume.
2. Xác nhận MPlayer dùng speaker mặc định.
3. Nếu có thiết bị, test Bluetooth và USB audio qua `.asoundrc` của NextUI.
4. Thoát khi đang phát: MPlayer dừng và NextUI trở lại sạch.
5. Mở lại: tài khoản, settings và artwork cache còn nguyên.
6. Cập nhật Pak: dữ liệu trong `.userdata` không bị mất.

## Tiêu chí báo cáo

- `BUILD_PASS`: source và cross-build thành công.
- `PACKAGE_PASS`: cây Tool Pak, shell syntax, ELF, GLIBC và ZIP hợp lệ.
- `NEXTUI_LAUNCH_PASS`: mở/thoát và trả về NextUI thành công trên thiết bị.
- `NEXTUI_INPUT_PASS`: toàn bộ phím, gồm volume +/-, đã thử thật.
- `NEXTUI_AUDIO_PASS`: phát thật qua loa; Bluetooth/USB ghi riêng nếu được thử.
- `NEXTUI_PERSISTENCE_PASS`: đóng/mở lại vẫn giữ tài khoản.
- Bất kỳ mục nào chưa chạy trên Brick phải ghi `NOT_TESTED`, không suy ra từ
  StockOS.

## Rủi ro còn lại

1. `EVIOCGRAB` có thể chặn volume nếu KEY_POWER và volume cùng event device.
   Đây là phép thử thiết bị quan trọng nhất và có thể sửa cục bộ trong input
   adapter.
2. NextUI không cung cấp lifecycle sleep cho standalone Pak. Bản đầu giữ màn
   hình hoạt động và không hỗ trợ background audio/deep sleep.
3. MPlayer được suy ra là còn dùng được vì NextUI giữ stock rootfs và thêm
   `/usr/trimui/bin` vào PATH; cần xác nhận bằng chạy thật trước khi đánh dấu
   audio pass.
4. Route Bluetooth/USB phụ thuộc file `.userdata/tg5040/.asoundrc` do
   `audiomon.elf` tạo. Loa mặc định phải test trước, các route ngoài báo cáo
   riêng.

## Phạm vi không thực hiện trong bước nghiên cứu

- Không sửa source Brickwave.
- Không đóng gói NextUI.
- Không chép lên thẻ hoặc qua SSH.
- Không sửa NextUI, firmware hay `.system`.
- Không thay MPlayer, SDL2, egui, backend Cloudflare hoặc SoundCloud API.
