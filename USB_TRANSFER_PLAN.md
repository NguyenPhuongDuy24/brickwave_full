# Phương án chép bản Brickwave qua USB

## Kết luận

Không nên dùng USB Mass Storage làm đường cập nhật mặc định. StockOS xuất trực
tiếp toàn bộ `/dev/mmcblk1` cho Windows, nên chỉ một lần thoát ứng dụng, rút cáp
hoặc mất điện trước khi Windows Eject xong cũng có thể để FAT ở trạng thái bẩn.

Phương án phù hợp nhất là **ADB qua cùng cáp USB**, đóng gói thành một script cài
đặt một lần bấm trên Windows. Thẻ vẫn chỉ được StockOS mount; Windows không giữ
raw block device. Người dùng vẫn cắm cáp và chép file như USB, nhưng không cần
tháo thẻ và không đi qua Mass Storage.

## Bằng chứng từ StockOS

Launcher gốc tại
`/work/rootfs/usr/trimui/apps/usb_storage/launch.sh`
có các điểm sau:

1. Gọi `umount /mnt/SDCARD` nhưng không kiểm tra mã lỗi trước khi export.
2. Chạy cả `fsck.fat -a` và `fsck.exfat` trên cùng thiết bị, bất kể filesystem.
3. `/bin/setusbconfig mass_storage,adb` gán toàn bộ `/dev/mmcblk1` làm LUN.
4. Khi thoát UI, launcher chạy fsck lần nữa **trước khi** chuyển gadget khỏi
   Mass Storage. Nếu Windows chưa Eject hoặc còn cache ghi, hai phía có thể cùng
   chạm block device.
5. Mount lại bằng `errors=continue` và không xác minh hậu điều kiện.

StockOS đã có `/bin/adbd`, dịch vụ `/etc/init.d/adbd` và cấu hình USB ADB mặc
định. BusyBox firmware cũng có `sha256sum`, đủ để kiểm tra gói sau khi push.

Dự án cộng đồng
[`trimui-brick-usb-mass-storage-pak`](https://github.com/josegonzalez/trimui-brick-usb-mass-storage-pak)
chỉ bọc launcher gốc và cũng cảnh báo nguy cơ hỏng SD; README của dự án khuyên
dùng ADB hoặc WebADB.

## Phương án A — khuyến nghị: Brickwave USB Installer qua ADB

Tạo `Install-Brickwave.ps1` kèm Android platform-tools hoặc hướng dẫn dùng
WebADB. Luồng cài đặt:

1. Kiểm tra đúng một thiết bị TrimUI qua `adb devices`.
2. Kiểm tra Brickwave không còn chạy.
3. Push payload vào thư mục version mới, ví dụ
   `/mnt/SDCARD/Apps/Brickwave/releases/00.4.6.pending`.
4. Chạy `sha256sum -c manifest.sha256` trên Brick.
5. Đổi tên `.pending` thành `00.4.6` trên chính thẻ.
6. Ghi `current.version.new`, gọi `sync`, rồi đổi tên thành `current.version`.
7. Giữ nguyên `data/`, `logs/` và bản release trước để rollback.

Launcher ổn định ở `Apps/Brickwave/launch.sh` đọc `current.version` và chạy
binary trong `releases/<version>/`. FAT không hỗ trợ symlink, vì vậy dùng file
text chỉ phiên bản. Nếu version mới thiếu `READY` hoặc sai manifest, launcher
quay về version trước.

Ưu điểm:

- Không unmount hoặc export raw SD cho Windows.
- Có thể chép lại nhanh bằng một file PowerShell.
- Không ghi đè binary đang dùng.
- Xác minh hash trước khi kích hoạt.
- Session đăng nhập trong `data/` không bị đụng tới.
- Rollback chỉ cần đổi `current.version`.

Hạn chế:

- PC hiện chưa có `adb`; cần cài platform-tools/driver một lần, hoặc dùng
  Chrome/Edge với WebADB.
- Cần một lần chuyển cấu trúc Brickwave sang `releases/<version>`.
- Phải kiểm thử driver và quyền ADB trên Brick thật trước khi phát hành script.

## Phương án B — vẫn dùng app USB Storage gốc

Chỉ dùng như giải pháp tạm thời với quy trình bắt buộc:

1. Thoát Brickwave và mọi app đang dùng SD.
2. Mở USB Storage từ MainUI và chờ Windows nhận ổ.
3. Chép payload vào một thư mục version mới; không ghi trực tiếp lên binary
   hiện tại. Chép file `READY` sau cùng.
4. So SHA-256 trên Windows.
5. Chọn **Safely Remove/Eject** trong Windows và chờ ổ biến mất hoàn toàn.
6. Chỉ sau đó mới bấm B/thoát màn hình USB Storage.
7. Không rút cáp, tắt máy hoặc thoát app trong lúc Windows còn thấy ổ.

Luồng này giảm rủi ro cập nhật dở nhưng không sửa được lỗi handoff của launcher
StockOS. Nếu Windows báo “Format disk”, phải Cancel; không format và không chạy
repair trong lúc Brick còn export thẻ.

## Phương án C — thay launcher USB Storage

Workspace đã có prototype tại
`D:/volte/spotify_port_analysis/usb_storage_trial`, gồm kiểm tra unmount,
UDC/LUN, fsck read-only và 52 ca mock flow. Prototype chưa thể phát hành vì:

- BusyBox firmware thiếu `stat` mà script đang dùng.
- Chưa khóa được auto-mount/hotplug theo cách nguyên tử.
- Chưa xác minh detach/LUN drain trên kernel vendor bằng thẻ phụ.
- Payload chạy từ SD không thể tự unmount volume chứa chính nó; cần stage toàn
  bộ launcher/UI vào RAM hoặc bộ nhớ trong.
- Nhánh lỗi chưa bảo đảm phục hồi ADB/USB role trong mọi trường hợp.

Chỉ nên tiếp tục phương án này nếu mục tiêu là sửa USB Mass Storage cho toàn hệ
thống, và phải thử bằng thẻ phụ không chứa dữ liệu quan trọng. Nó không phải
đường ngắn nhất để cập nhật Brickwave.

## Đề xuất thực hiện

1. Giữ USB Storage gốc làm phương án thủ công có Eject bắt buộc.
2. Làm `Install-Brickwave.ps1` qua ADB và cấu trúc release versioned.
3. Thử ADB trên Brick thật: nhận thiết bị, push file nhỏ, hash, rename, sync và
   khởi động app.
4. Sau khi pass, đóng gói installer cùng mỗi ZIP. Không thay firmware và không
   đụng launcher USB Storage gốc.

Trạng thái hiện tại: **RESEARCH_COMPLETE / IMPLEMENTATION_NOT_STARTED /
DEVICE_NOT_TESTED**.
