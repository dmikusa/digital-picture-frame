"""
Digital Picture Frame - Photo Import Module Tests
Copyright (C) 2025 Daniel Mikusa

This program is free software: you can redistribute it and/or modify
it under the terms of the GNU Affero General Public License as published
by the Free Software Foundation, either version 3 of the License, or
(at your option) any later version.

This program is distributed in the hope that it will be useful,
but WITHOUT ANY WARRANTY; without even the implied warranty of
MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
GNU Affero General Public License for more details.

You should have received a copy of the GNU Affero General Public License
along with this program.  If not, see <https://www.gnu.org/licenses/>.
"""

import tempfile
import pytest
from pathlib import Path
from PIL import Image

from picture_frame_ui.importer import PhotoImporter, import_photos_from_directory


class TestPhotoImporter:
    """Test cases for PhotoImporter class"""

    @pytest.fixture
    def temp_dirs(self):
        """Create temporary directories for testing"""
        with tempfile.TemporaryDirectory() as temp_dir:
            temp_path = Path(temp_dir)
            import_dir = temp_path / "import"
            photos_dir = temp_path / "photos"
            import_dir.mkdir()
            photos_dir.mkdir()
            yield import_dir, photos_dir

    @pytest.fixture
    def sample_image(self, temp_dirs):
        """Create a sample image for testing"""
        import_dir, _ = temp_dirs
        image_path = import_dir / "test_image.jpg"

        # Create a test image
        img = Image.new("RGB", (2400, 1600), color="red")
        img.save(image_path, "JPEG")

        return image_path

    @pytest.fixture
    def small_image(self, temp_dirs):
        """Create a small image that doesn't need resizing"""
        import_dir, _ = temp_dirs
        image_path = import_dir / "small_image.jpg"

        # Create a small test image
        img = Image.new("RGB", (800, 600), color="blue")
        img.save(image_path, "JPEG")

        return image_path

    def test_calculate_sha1(self, temp_dirs, sample_image):
        """Test SHA1 hash calculation"""
        import_dir, photos_dir = temp_dirs
        importer = PhotoImporter(import_dir, photos_dir)

        hash1 = importer.calculate_sha1(sample_image)
        hash2 = importer.calculate_sha1(sample_image)

        # Hash should be consistent
        assert hash1 == hash2
        assert len(hash1) == 40  # SHA1 produces 40-character hex string
        assert all(c in "0123456789abcdef" for c in hash1)

    def test_get_image_dimensions(self, temp_dirs, sample_image):
        """Test getting image dimensions"""
        import_dir, photos_dir = temp_dirs
        importer = PhotoImporter(import_dir, photos_dir)

        width, height = importer.get_image_dimensions(sample_image)
        assert width == 2400
        assert height == 1600

    def test_calculate_resize_dimensions_needs_resize(self, temp_dirs):
        """Test dimension calculation when resize is needed"""
        import_dir, photos_dir = temp_dirs
        importer = PhotoImporter(import_dir, photos_dir)

        # Test image larger than max dimensions
        new_width, new_height = importer.calculate_resize_dimensions(2400, 1600)

        # Should scale down to fit within 1920x1080
        assert new_width <= 1920
        assert new_height <= 1080

        # Should maintain aspect ratio (2400:1600 = 1.5:1)
        aspect_ratio = new_width / new_height
        expected_ratio = 2400 / 1600
        assert abs(aspect_ratio - expected_ratio) < 0.01

    def test_calculate_resize_dimensions_custom_screen_size(self, temp_dirs):
        """Test dimension calculation with custom screen dimensions"""
        import_dir, photos_dir = temp_dirs
        # Test with a smaller screen size
        importer = PhotoImporter(import_dir, photos_dir, max_width=1366, max_height=768)

        # Test image larger than custom max dimensions
        new_width, new_height = importer.calculate_resize_dimensions(2400, 1600)

        # Should scale down to fit within 1366x768
        assert new_width <= 1366
        assert new_height <= 768

        # Should maintain aspect ratio (2400:1600 = 1.5:1)
        aspect_ratio = new_width / new_height
        expected_ratio = 2400 / 1600
        assert abs(aspect_ratio - expected_ratio) < 0.01

        # For this specific case, height should be the limiting factor
        # 1366/768 = 1.78, 2400/1600 = 1.5, so height is more restrictive
        assert new_height == 768
        assert new_width == int(768 * (2400 / 1600))  # 1152

    def test_calculate_resize_width_needs_resize(self, temp_dirs):
        """Test dimension calculation when width resize is needed"""
        import_dir, photos_dir = temp_dirs
        importer = PhotoImporter(import_dir, photos_dir)

        # Test image larger than max dimensions
        new_width, new_height = importer.calculate_resize_dimensions(2400, 900)

        # Should scale down to fit within 1920x1080
        assert new_width <= 1920
        assert new_height <= 1080

        # Should maintain aspect ratio (2400:900 = 2.67:1)
        aspect_ratio = new_width / new_height
        expected_ratio = 2400 / 900
        assert abs(aspect_ratio - expected_ratio) < 0.01

    def test_calculate_resize_height_needs_resize(self, temp_dirs):
        """Test dimension calculation when height resize is needed"""
        import_dir, photos_dir = temp_dirs
        importer = PhotoImporter(import_dir, photos_dir)

        # Test image larger than max dimensions
        new_width, new_height = importer.calculate_resize_dimensions(900, 2400)

        # Should scale down to fit within 1920x1080
        assert new_width <= 1920
        assert new_height <= 1080

        # Should maintain aspect ratio (900:2400 = 0.375:1)
        aspect_ratio = new_width / new_height
        expected_ratio = 900 / 2400
        assert abs(aspect_ratio - expected_ratio) < 0.01

        # Should scale down to fit within 1920x1080
        assert new_width <= 1920
        assert new_height <= 1080

        # Should maintain aspect ratio (900:2400 = 0.375:1)
        aspect_ratio = new_width / new_height
        expected_ratio = 900 / 2400
        assert abs(aspect_ratio - expected_ratio) < 0.01

    def test_calculate_resize_dimensions_no_resize_needed(self, temp_dirs):
        """Test dimension calculation when no resize is needed"""
        import_dir, photos_dir = temp_dirs
        importer = PhotoImporter(import_dir, photos_dir)

        # Test image smaller than max dimensions
        new_width, new_height = importer.calculate_resize_dimensions(800, 600)

        # Should return original dimensions
        assert new_width == 800
        assert new_height == 600

    def test_calculate_resize_dimensions_tall_image(self, temp_dirs):
        """Test dimension calculation for tall images"""
        import_dir, photos_dir = temp_dirs
        importer = PhotoImporter(import_dir, photos_dir)

        # Test tall image (height is limiting factor)
        new_width, new_height = importer.calculate_resize_dimensions(1080, 1920)

        # Height should be at max, width should be scaled down
        assert new_height == 1080  # Limited by MAX_HEIGHT
        assert new_width < 1920

        # Should maintain aspect ratio
        aspect_ratio = new_width / new_height
        expected_ratio = 1080 / 1920
        assert abs(aspect_ratio - expected_ratio) < 0.01

    def test_generate_target_filename(self, temp_dirs, sample_image):
        """Test target filename generation"""
        import_dir, photos_dir = temp_dirs
        importer = PhotoImporter(import_dir, photos_dir)

        hash_value = "abcdef1234567890"
        filename = importer.generate_target_filename(sample_image, hash_value)

        expected = f"test_image##{hash_value}.jpg"
        assert filename == expected

    def test_photo_exists_in_directory_false(self, temp_dirs):
        """Test checking for non-existent photo"""
        import_dir, photos_dir = temp_dirs
        importer = PhotoImporter(import_dir, photos_dir)

        assert not importer.photo_exists_in_directory("nonexistent_hash")

    def test_photo_exists_in_directory_true(self, temp_dirs):
        """Test checking for existing photo"""
        import_dir, photos_dir = temp_dirs
        importer = PhotoImporter(import_dir, photos_dir)

        # Create a file with hash pattern
        hash_value = "abcdef1234567890"
        existing_file = photos_dir / f"existing##{ hash_value}.jpg"
        existing_file.touch()

        assert importer.photo_exists_in_directory(hash_value)

    def test_process_photo_success_with_resize(self, temp_dirs, sample_image):
        """Test successful photo processing with resize"""
        import_dir, photos_dir = temp_dirs
        importer = PhotoImporter(import_dir, photos_dir)

        result = importer.process_photo(sample_image)

        assert result is True

        # Check that a file was created in photos directory
        photo_files = list(photos_dir.glob("*.jpg"))
        assert len(photo_files) == 1

        # Verify the file has the correct naming pattern
        created_file = photo_files[0]
        assert "##" in created_file.name
        assert created_file.name.startswith("test_image##")
        assert created_file.name.endswith(".jpg")

        # Verify the image was resized
        with Image.open(created_file) as img:
            width, height = img.size
            assert width <= 1920
            assert height <= 1080

    def test_process_photo_success_no_resize(self, temp_dirs, small_image):
        """Test successful photo processing without resize"""
        import_dir, photos_dir = temp_dirs
        importer = PhotoImporter(import_dir, photos_dir)

        result = importer.process_photo(small_image)

        assert result is True

        # Check that a file was created in photos directory
        photo_files = list(photos_dir.glob("*.jpg"))
        assert len(photo_files) == 1

        # Verify the image dimensions are preserved (or close to original)
        created_file = photo_files[0]
        with Image.open(created_file) as img:
            width, height = img.size
            # Should be same or very close to original (800x600)
            assert 795 <= width <= 805  # Allow for slight compression differences
            assert 595 <= height <= 605

    def test_process_photo_skip_existing(self, temp_dirs, sample_image):
        """Test skipping photo that already exists"""
        import_dir, photos_dir = temp_dirs
        importer = PhotoImporter(import_dir, photos_dir)

        # Process photo first time
        result1 = importer.process_photo(sample_image)
        assert result1 is True

        # Process same photo again - should be skipped
        result2 = importer.process_photo(sample_image)
        assert result2 is False

        # Should still only have one file
        photo_files = list(photos_dir.glob("*.jpg"))
        assert len(photo_files) == 1

    def test_import_photos_empty_directory(self, temp_dirs):
        """Test importing from empty directory"""
        import_dir, photos_dir = temp_dirs
        importer = PhotoImporter(import_dir, photos_dir)

        count = importer.import_photos()
        assert count == 0

    def test_import_photos_multiple_files(self, temp_dirs):
        """Test importing multiple photos"""
        import_dir, photos_dir = temp_dirs

        # Create multiple test images
        for i in range(3):
            img = Image.new("RGB", (1000 + i * 100, 800), color="green")
            img.save(import_dir / f"image_{i}.jpg", "JPEG")

        # Create a non-image file (should be ignored)
        (import_dir / "readme.txt").write_text("Not an image")

        importer = PhotoImporter(import_dir, photos_dir)
        count = importer.import_photos()

        assert count == 3

        # Check that 3 files were created
        photo_files = list(photos_dir.glob("*.jpg"))
        assert len(photo_files) == 3

    def test_import_photos_nonexistent_directory(self, temp_dirs):
        """Test importing from non-existent directory"""
        _, photos_dir = temp_dirs
        nonexistent_dir = Path("/nonexistent/directory")

        importer = PhotoImporter(nonexistent_dir, photos_dir)
        count = importer.import_photos()

        assert count == 0

    def test_import_photos_supported_formats(self, temp_dirs):
        """Test importing various supported image formats"""
        import_dir, photos_dir = temp_dirs

        # Create images in different formats
        formats = [
            ("image.jpg", "JPEG"),
            ("image.png", "PNG"),
            ("image.bmp", "BMP"),
            ("image.tiff", "TIFF"),
        ]

        for filename, format_name in formats:
            img = Image.new("RGB", (800, 600), color="yellow")
            img.save(import_dir / filename, format_name)

        importer = PhotoImporter(import_dir, photos_dir)
        count = importer.import_photos()

        assert count == len(formats)

        # All should be converted to JPEG in output
        photo_files = list(photos_dir.glob("*.jpg"))
        assert len(photo_files) == len(formats)


class TestConvenienceFunction:
    """Test the convenience function"""

    def test_import_photos_from_directory(self):
        """Test the convenience function"""
        with tempfile.TemporaryDirectory() as temp_dir:
            temp_path = Path(temp_dir)
            import_dir = temp_path / "import"
            photos_dir = temp_path / "photos"
            import_dir.mkdir()
            photos_dir.mkdir()

            # Create test image
            img = Image.new("RGB", (1200, 800), color="purple")
            img.save(import_dir / "test.jpg", "JPEG")

            count = import_photos_from_directory(import_dir, photos_dir)

            assert count == 1
            photo_files = list(photos_dir.glob("*.jpg"))
            assert len(photo_files) == 1

    def test_import_photos_from_directory_custom_dimensions(self):
        """Test the convenience function with custom screen dimensions"""
        with tempfile.TemporaryDirectory() as temp_dir:
            temp_path = Path(temp_dir)
            import_dir = temp_path / "import"
            photos_dir = temp_path / "photos"
            import_dir.mkdir()
            photos_dir.mkdir()

            # Create test image that would need resizing with smaller screen
            img = Image.new("RGB", (2560, 1440), color="purple")
            img.save(import_dir / "test.jpg", "JPEG")

            # Import with custom small screen dimensions
            count = import_photos_from_directory(
                import_dir, photos_dir, max_width=1024, max_height=768
            )

            assert count == 1
            photo_files = list(photos_dir.glob("*.jpg"))
            assert len(photo_files) == 1

            # Verify the image was resized to fit the smaller screen
            created_file = photo_files[0]
            with Image.open(created_file) as resized_img:
                width, height = resized_img.size
                assert width <= 1024
                assert height <= 768
                # Should maintain aspect ratio
                original_ratio = 2560 / 1440
                new_ratio = width / height
                assert abs(original_ratio - new_ratio) < 0.01


class TestErrorHandling:
    """Test error handling scenarios"""

    @pytest.fixture
    def temp_dirs(self):
        """Create temporary directories for testing"""
        with tempfile.TemporaryDirectory() as temp_dir:
            temp_path = Path(temp_dir)
            import_dir = temp_path / "import"
            photos_dir = temp_path / "photos"
            import_dir.mkdir()
            photos_dir.mkdir()
            yield import_dir, photos_dir

    @pytest.fixture
    def sample_image(self, temp_dirs):
        """Create a sample image for testing"""
        import_dir, _ = temp_dirs
        image_path = import_dir / "test_image.jpg"

        # Create a test image
        img = Image.new("RGB", (2400, 1600), color="red")
        img.save(image_path, "JPEG")

        return image_path

    def test_process_invalid_image_file(self, temp_dirs):
        """Test processing an invalid image file"""
        import_dir, photos_dir = temp_dirs

        # Create a file with image extension but invalid content
        invalid_file = import_dir / "invalid.jpg"
        invalid_file.write_text("This is not an image")

        importer = PhotoImporter(import_dir, photos_dir)
        result = importer.process_photo(invalid_file)

        assert result is False

        # No files should be created
        photo_files = list(photos_dir.glob("*.jpg"))
        assert len(photo_files) == 0

    def test_process_photo_permission_error(self, temp_dirs, sample_image):
        """Test handling permission errors when writing files"""
        import_dir, photos_dir = temp_dirs

        # Make photos directory read-only
        photos_dir.chmod(0o444)

        try:
            importer = PhotoImporter(import_dir, photos_dir)
            result = importer.process_photo(sample_image)

            # Should handle the error gracefully
            assert result is False
        finally:
            # Restore permissions for cleanup
            photos_dir.chmod(0o755)
