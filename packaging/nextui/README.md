# Brickwave cho NextUI 00.2.0

Gói này chứa Brickwave 0.4.25 dành cho **TrimUI Brick Pro chạy NextUI**. Gói được tách riêng với bản StockOS trong `Apps/Brickwave`.

## Cài đặt

1. Giải nén file ZIP trên máy tính.
2. Sao chép thư mục `Tools` vào thư mục gốc của thẻ NextUI và gộp với thư mục `Tools` đang có.
3. Kiểm tra launcher tại `Tools/tg5040/Brickwave.pak/launch.sh`.
4. Tháo thẻ an toàn, khởi động NextUI, mở **Tools** rồi chọn **Brickwave**.

Dữ liệu đăng nhập và cài đặt nằm tại `.userdata/shared/BrickwaveNextUI/data`. Cache ảnh nằm tại `.userdata/shared/BrickwaveNextUI/artwork-cache`. Log nằm tại `.userdata/tg5040/logs/brickwave-nextui.log`. Việc cập nhật Pak không ghi đè các đường dẫn này.

## Phím điều khiển

- Analog: di chuyển con trỏ ảo.
- D-pad: cuộn danh sách; di chuyển lựa chọn trên bàn phím ảo.
- A: bấm, xác nhận hoặc giữ để kéo.
- B: quay lại hoặc hủy.
- MENU: mở hộp thoại xác nhận thoát; A thoát và B hủy.
- Y/SELECT: tạm dừng.
- START: phát hoặc tiếp tục.
- L1/R1: bài trước hoặc bài tiếp theo.
- POWER: do NextUI xử lý.

## Hoạt động trên NextUI

Brickwave chạy dưới dạng Tool Pak độc lập. Ứng dụng không gọi IPC chỉnh độ sáng của StockOS, không tạo cờ chống ngủ của StockOS và không thay đổi trạng thái LED. Launcher giữ nguyên `HOME` và `.asoundrc` của NextUI để dùng đúng đường âm thanh của hệ thống.

Pak không tích hợp quick-save hoặc Quick Menu của minarch. Chế độ nguồn và sleep vẫn cần kiểm thử trên TrimUI Brick Pro chạy NextUI trước khi được coi là đã hỗ trợ đầy đủ.

NextUI không hỗ trợ chính thức các Pak bên thứ ba. Hãy báo lỗi Brickwave tại dự án Brickwave, không gửi lỗi Brickwave tới nhóm phát triển NextUI.

Trạng thái: BUILD_PASS / HOST_TEST_PASS / NEXTUI_DEVICE_NOT_TESTED.
