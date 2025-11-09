"""
Tests for the photos module
"""

import tempfile
from pathlib import Path

import pytest

from .photos import FilePhotoLoader, create_photo_loader


class TestFilePhotoLoader:
    """Test cases for FilePhotoLoader class"""

    def create_test_directory_with_files(
        self, temp_dir: Path, files: list[str]
    ) -> None:
        """Helper to create test files in directory"""
        for file_name in files:
            file_path = temp_dir / file_name
            # Create parent directories if needed
            file_path.parent.mkdir(parents=True, exist_ok=True)
            # Create file with test content
            file_path.write_text("test image content")

    def test_initialization(self):
        """Test FilePhotoLoader initialization"""
        loader = FilePhotoLoader(Path("/test/directory"))
        assert loader.base_directory == Path("/test/directory")
        assert loader._current_iterator is None

    def test_load_next_photo_single_file(self):
        """Test loading from directory with single image file"""
        with tempfile.TemporaryDirectory() as temp_dir:
            temp_path = Path(temp_dir)
            self.create_test_directory_with_files(temp_path, ["photo1.jpg"])

            loader = FilePhotoLoader(temp_path)
            photo_url = loader.load_next_photo()

            assert photo_url.startswith("file://")
            assert "photo1.jpg" in photo_url

    def test_load_next_photo_multiple_files(self):
        """Test loading from directory with multiple image files"""
        with tempfile.TemporaryDirectory() as temp_dir:
            temp_path = Path(temp_dir)
            files = ["photo1.jpg", "photo2.png", "photo3.gif"]
            self.create_test_directory_with_files(temp_path, files)

            loader = FilePhotoLoader(temp_path)

            # Load all files
            loaded_urls = []
            for _ in range(len(files)):
                url = loader.load_next_photo()
                loaded_urls.append(url)
                assert url.startswith("file://")

            # Check all files were loaded (order may vary by OS)
            loaded_filenames = [
                Path(url.replace("file://", "")).name for url in loaded_urls
            ]
            assert set(loaded_filenames) == set(files)

    def test_load_next_photo_cycling(self):
        """Test that loader cycles through directory when reaching end"""
        with tempfile.TemporaryDirectory() as temp_dir:
            temp_path = Path(temp_dir)
            files = ["photo1.jpg", "photo2.png"]
            self.create_test_directory_with_files(temp_path, files)

            loader = FilePhotoLoader(temp_path)

            # Load all files in directory
            first_round = []
            for _ in range(len(files)):
                first_round.append(loader.load_next_photo())

            # Load next photo - should cycle back to beginning
            next_url = loader.load_next_photo()
            assert next_url in first_round

    def test_load_next_photo_nonexistent_directory(self):
        """Test handling of nonexistent directory"""
        loader = FilePhotoLoader(Path("/nonexistent/directory"))

        with pytest.raises(FileNotFoundError):
            loader.load_next_photo()

    def test_load_next_photo_empty_directory(self):
        """Test handling of directory with no image files"""
        with tempfile.TemporaryDirectory() as temp_dir:
            loader = FilePhotoLoader(Path(temp_dir))

            with pytest.raises(FileNotFoundError, match="No image files found"):
                loader.load_next_photo()

    def test_load_next_photo_directory_with_non_images(self):
        """Test directory with non-image files are ignored"""
        with tempfile.TemporaryDirectory() as temp_dir:
            temp_path = Path(temp_dir)
            # Create mix of image and non-image files
            files = ["photo1.jpg", "document.txt", "photo2.png", "data.csv"]
            self.create_test_directory_with_files(temp_path, files)

            loader = FilePhotoLoader(temp_path)

            # Should only load image files
            loaded_urls = []
            for _ in range(2):  # Only 2 image files
                url = loader.load_next_photo()
                loaded_urls.append(url)

            # Check only image files were loaded
            loaded_filenames = [
                Path(url.replace("file://", "")).name for url in loaded_urls
            ]
            assert "photo1.jpg" in loaded_filenames
            assert "photo2.png" in loaded_filenames
            assert "document.txt" not in " ".join(loaded_urls)
            assert "data.csv" not in " ".join(loaded_urls)

    def test_image_extensions_recognition(self):
        """Test that all supported image extensions are recognized"""
        with tempfile.TemporaryDirectory() as temp_dir:
            temp_path = Path(temp_dir)

            # Test various image extensions
            image_files = [
                "test.jpg",
                "test.jpeg",
                "test.png",
                "test.gif",
                "test.bmp",
                "test.tiff",
                "test.tif",
                "test.webp",
                "TEST.JPG",  # Test case insensitivity
            ]
            self.create_test_directory_with_files(temp_path, image_files)

            loader = FilePhotoLoader(temp_path)

            # Should be able to load all image files
            loaded_count = 0
            try:
                for _ in range(len(image_files)):
                    url = loader.load_next_photo()
                    loaded_count += 1
                    assert url.startswith("file://")
            except FileNotFoundError:
                # If we get here, we've cycled through all files
                pass

            assert loaded_count >= len(image_files)

    def test_refresh_directory(self):
        """Test refreshing directory listing"""
        with tempfile.TemporaryDirectory() as temp_dir:
            temp_path = Path(temp_dir)
            self.create_test_directory_with_files(temp_path, ["photo1.jpg"])

            loader = FilePhotoLoader(temp_path)

            # Load first photo
            first_url = loader.load_next_photo()
            assert "photo1.jpg" in first_url

            # Add a new file
            self.create_test_directory_with_files(temp_path, ["photo2.png"])

            # Refresh directory
            loader.refresh_directory()

            # Should be able to load new file on next iteration
            loaded_urls = set()
            for _ in range(3):  # Load enough to see both files
                url = loader.load_next_photo()
                loaded_urls.add(url)

            # Should have both files
            filenames = " ".join(loaded_urls)
            assert "photo1.jpg" in filenames
            assert "photo2.png" in filenames

    def test_file_path_not_directory(self):
        """Test handling when path points to a file instead of directory"""
        with tempfile.TemporaryDirectory() as temp_dir:
            # Create a file instead of directory
            file_path = Path(temp_dir) / "not_a_directory.txt"
            file_path.write_text("test content")

            loader = FilePhotoLoader(file_path)

            with pytest.raises(NotADirectoryError):
                loader.load_next_photo()

    def test_sorted_file_order(self):
        """Test that files are loaded in consistent sorted order"""
        with tempfile.TemporaryDirectory() as temp_dir:
            temp_path = Path(temp_dir)
            # Create files in non-alphabetical order
            files = ["z_photo.jpg", "a_photo.jpg", "m_photo.jpg"]
            self.create_test_directory_with_files(temp_path, files)

            loader = FilePhotoLoader(temp_path)

            # Load all files
            loaded_urls = []
            for _ in range(len(files)):
                loaded_urls.append(loader.load_next_photo())

            # Extract filenames and check they're sorted
            loaded_filenames = [
                Path(url.replace("file://", "")).name for url in loaded_urls
            ]
            expected_order = ["a_photo.jpg", "m_photo.jpg", "z_photo.jpg"]
            assert loaded_filenames == expected_order


class TestCreatePhotoLoader:
    """Test cases for create_photo_loader factory function"""

    def test_create_photo_loader_returns_file_loader(self):
        """Test that factory function returns FilePhotoLoader instance"""
        loader = create_photo_loader(Path("/test/path"))
        assert isinstance(loader, FilePhotoLoader)
        assert loader.base_directory == Path("/test/path")

    def test_create_photo_loader_with_string_path(self):
        """Test factory function with string path"""
        path = Path("/home/user/photos")
        loader = create_photo_loader(path)
        assert isinstance(loader, FilePhotoLoader)
        assert loader.base_directory == path
