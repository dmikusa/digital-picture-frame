"""
Tests for the config module
"""

import json
import os
import tempfile
from pathlib import Path
from unittest.mock import patch


from .config import FrameConfig


class TestFrameConfig:
    """Test cases for FrameConfig class"""

    def test_default_config(self):
        """Test that default configuration has expected values"""
        config = FrameConfig()
        assert config.photos_directory == "images"
        assert config.slideshow_duration == 5
        assert config.fade_duration == 1000
        assert config.import_directory is None
        assert config.full_screen is False

    def test_config_serialization(self):
        """Test converting config to dictionary"""
        config = FrameConfig(
            photos_directory="/home/user/photos",
            slideshow_duration=10,
            fade_duration=2000,
            import_directory="/home/user/import",
            full_screen=True,
        )

        config_dict = config.to_dict()
        expected = {
            "photos_directory": "/home/user/photos",
            "slideshow_duration": 10,
            "fade_duration": 2000,
            "import_directory": "/home/user/import",
            "full_screen": True,
            "rendering_type": "GPU",
            "server_host": "0.0.0.0",
            "server_port": 3400,
            "server_max_file_size": 20971520,
        }
        assert config_dict == expected

    def test_load_from_file_current_directory(self):
        """Test loading config from current directory"""
        with tempfile.TemporaryDirectory() as temp_dir:
            # Change to temp directory
            original_cwd = os.getcwd()
            try:
                os.chdir(temp_dir)

                # Create config file
                config_path = Path("frame-config.json")
                test_config = {
                    "photos_directory": "/test/photos",
                    "slideshow_duration": 8,
                    "fade_duration": 1500,
                    "import_directory": "/test/import",
                }

                with open(config_path, "w") as f:
                    json.dump(test_config, f)

                # Load config
                config = FrameConfig.load()

                assert config.photos_directory == "/test/photos"
                assert config.slideshow_duration == 8
                assert config.fade_duration == 1500
                assert config.import_directory == "/test/import"

            finally:
                os.chdir(original_cwd)

    def test_load_from_file_home_directory(self):
        """Test loading config from home directory"""
        with tempfile.TemporaryDirectory() as temp_dir:
            # Mock HOME environment variable
            with patch.dict(os.environ, {"HOME": temp_dir}):
                # Create config directory and file
                config_dir = Path(temp_dir) / ".picture-frame-ui"
                config_dir.mkdir()
                config_path = config_dir / "frame-config.json"

                test_config = {
                    "photos_directory": "/home/test/photos",
                    "slideshow_duration": 3,
                    "fade_duration": 500,
                }

                with open(config_path, "w") as f:
                    json.dump(test_config, f)

                # Change to different directory to ensure it finds home config
                with tempfile.TemporaryDirectory() as other_dir:
                    original_cwd = os.getcwd()
                    try:
                        os.chdir(other_dir)
                        config = FrameConfig.load()

                        assert config.photos_directory == "/home/test/photos"
                        assert config.slideshow_duration == 3
                        assert config.fade_duration == 500

                    finally:
                        os.chdir(original_cwd)

    def test_load_defaults_when_no_config_file(self):
        """Test that defaults are used when no config file exists"""
        with tempfile.TemporaryDirectory() as temp_dir:
            # Mock HOME to a directory without config
            with patch.dict(os.environ, {"HOME": temp_dir}):
                original_cwd = os.getcwd()
                try:
                    os.chdir(temp_dir)
                    config = FrameConfig.load()

                    assert config.photos_directory == "images"
                    assert config.slideshow_duration == 5
                    assert config.fade_duration == 1000
                    assert config.import_directory is None

                finally:
                    os.chdir(original_cwd)

    def test_load_from_invalid_json(self):
        """Test handling of invalid JSON in config file"""
        with tempfile.TemporaryDirectory() as temp_dir:
            original_cwd = os.getcwd()
            try:
                os.chdir(temp_dir)

                # Create invalid JSON file
                config_path = Path("frame-config.json")
                with open(config_path, "w") as f:
                    f.write("{ invalid json }")

                # Should fall back to defaults
                config = FrameConfig.load()

                assert config.photos_directory == "images"
                assert config.slideshow_duration == 5
                assert config.fade_duration == 1000
                assert config.import_directory is None

            finally:
                os.chdir(original_cwd)

    def test_load_with_unknown_config_options(self):
        """Test that unknown config options are ignored"""
        with tempfile.TemporaryDirectory() as temp_dir:
            original_cwd = os.getcwd()
            try:
                os.chdir(temp_dir)

                # Create config with unknown option
                config_path = Path("frame-config.json")
                test_config = {
                    "photos_directory": "/test/photos",
                    "slideshow_duration": 7,
                    "fade_duration": 800,
                    "unknown_option": "should be ignored",
                }

                with open(config_path, "w") as f:
                    json.dump(test_config, f)

                config = FrameConfig.load()

                assert config.photos_directory == "/test/photos"
                assert config.slideshow_duration == 7
                assert config.fade_duration == 800
                assert not hasattr(config, "unknown_option")

            finally:
                os.chdir(original_cwd)

    def test_get_photos_path_relative(self):
        """Test getting absolute path from relative photos directory"""
        with tempfile.TemporaryDirectory() as temp_dir:
            original_cwd = os.getcwd()
            try:
                os.chdir(temp_dir)
                # Create the photos directory
                photos_dir = Path("photos")
                photos_dir.mkdir()

                config = FrameConfig(photos_directory="photos")
                path = config.get_photos_path()

                assert path.is_absolute()
                assert path.name == "photos"
                assert path.exists()
            finally:
                os.chdir(original_cwd)

    def test_get_photos_path_absolute(self):
        """Test getting absolute path from absolute photos directory"""
        with tempfile.TemporaryDirectory() as temp_dir:
            # Create the photos directory
            photos_dir = Path(temp_dir) / "photos"
            photos_dir.mkdir()

            config = FrameConfig(photos_directory=str(photos_dir))
            path = config.get_photos_path()

            assert path == photos_dir
            assert path.is_absolute()
            assert path.exists()

    def test_get_photos_path_not_exists(self):
        """Test error when photos directory does not exist"""
        config = FrameConfig(photos_directory="/nonexistent/path")

        try:
            config.get_photos_path()
            assert False, "Expected FileNotFoundError"
        except FileNotFoundError as e:
            assert "Photos directory does not exist" in str(e)

    def test_get_photos_path_not_directory(self):
        """Test error when photos path is not a directory"""
        with tempfile.TemporaryDirectory() as temp_dir:
            # Create a file instead of a directory
            file_path = Path(temp_dir) / "not_a_dir.txt"
            file_path.touch()

            config = FrameConfig(photos_directory=str(file_path))

            try:
                config.get_photos_path()
                assert False, "Expected NotADirectoryError"
            except NotADirectoryError as e:
                assert "Photos path is not a directory" in str(e)

    def test_get_import_path_none(self):
        """Test getting import path when import_directory is None"""
        config = FrameConfig()
        path = config.get_import_path()

        assert path is None

    def test_get_import_path_relative(self):
        """Test getting absolute path from relative import directory"""
        config = FrameConfig(import_directory="import")
        path = config.get_import_path()

        assert path is not None
        assert path.is_absolute()
        assert path.name == "import"

    def test_get_import_path_absolute(self):
        """Test getting absolute path from absolute import directory"""
        abs_path = "/home/user/import"
        config = FrameConfig(import_directory=abs_path)
        path = config.get_import_path()

        assert path is not None
        assert path == Path(abs_path)
        assert path.is_absolute()

    def test_config_serialization_with_none_import(self):
        """Test serialization when import_directory is None"""
        config = FrameConfig()
        config_dict = config.to_dict()

        expected = {
            "photos_directory": "images",
            "slideshow_duration": 5,
            "fade_duration": 1000,
            "import_directory": None,
            "full_screen": False,
            "rendering_type": "GPU",
            "server_host": "0.0.0.0",
            "server_port": 3400,
            "server_max_file_size": 20971520,
        }
        assert config_dict == expected

    def test_full_screen_config(self):
        """Test full screen configuration"""
        # Test default (false)
        config = FrameConfig()
        assert config.full_screen is False

        # Test explicitly set to true
        config_true = FrameConfig(full_screen=True)
        assert config_true.full_screen is True

        # Test serialization includes full_screen
        config_dict = config_true.to_dict()
        assert config_dict["full_screen"] is True

    def test_rendering_type_default(self):
        """Test default rendering type is GPU"""
        config = FrameConfig()
        assert config.rendering_type == "GPU"

    def test_rendering_type_cpu(self):
        """Test setting rendering type to CPU"""
        config = FrameConfig(rendering_type="CPU")
        assert config.rendering_type == "CPU"

    def test_rendering_type_gpu(self):
        """Test setting rendering type to GPU"""
        config = FrameConfig(rendering_type="GPU")
        assert config.rendering_type == "GPU"

    def test_rendering_type_serialization(self):
        """Test rendering type is included in serialization"""
        config = FrameConfig(rendering_type="CPU")
        config_dict = config.to_dict()
        assert config_dict["rendering_type"] == "CPU"

    def test_load_with_invalid_rendering_type(self):
        """Test loading config with invalid rendering type falls back to GPU"""
        with tempfile.TemporaryDirectory() as temp_dir:
            # Change to temp directory
            original_cwd = os.getcwd()
            try:
                os.chdir(temp_dir)

                # Create config file with invalid rendering type
                config_path = Path("frame-config.json")
                test_config = {
                    "rendering_type": "INVALID",
                    "photos_directory": "/test/photos",
                }

                with open(config_path, "w") as f:
                    json.dump(test_config, f)

                # Load config - should default to GPU and log warning
                config = FrameConfig.load()
                assert config.rendering_type == "GPU"
                assert config.photos_directory == "/test/photos"

            finally:
                os.chdir(original_cwd)

    def test_load_with_rendering_type_cpu(self):
        """Test loading config with CPU rendering type"""
        with tempfile.TemporaryDirectory() as temp_dir:
            # Change to temp directory
            original_cwd = os.getcwd()
            try:
                os.chdir(temp_dir)

                # Create config file
                config_path = Path("frame-config.json")
                test_config = {
                    "rendering_type": "CPU",
                    "photos_directory": "/test/photos",
                }

                with open(config_path, "w") as f:
                    json.dump(test_config, f)

                # Load config
                config = FrameConfig.load()
                assert config.rendering_type == "CPU"
                assert config.photos_directory == "/test/photos"

            finally:
                os.chdir(original_cwd)
