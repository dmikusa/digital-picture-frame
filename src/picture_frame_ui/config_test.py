"""
Tests for the config module
"""

import json
import os
import tempfile
from pathlib import Path
from unittest.mock import patch

import pytest

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

    def test_config_serialization(self):
        """Test converting config to dictionary"""
        config = FrameConfig(
            photos_directory="/home/user/photos",
            slideshow_duration=10,
            fade_duration=2000,
            import_directory="/home/user/import",
        )

        config_dict = config.to_dict()
        expected = {
            "photos_directory": "/home/user/photos",
            "slideshow_duration": 10,
            "fade_duration": 2000,
            "import_directory": "/home/user/import",
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
        config = FrameConfig(photos_directory="photos")
        path = config.get_photos_path()

        assert path.is_absolute()
        assert path.name == "photos"

    def test_get_photos_path_absolute(self):
        """Test getting absolute path from absolute photos directory"""
        abs_path = "/home/user/photos"
        config = FrameConfig(photos_directory=abs_path)
        path = config.get_photos_path()

        assert path == Path(abs_path)
        assert path.is_absolute()

    def test_save_config(self):
        """Test saving configuration to file"""
        with tempfile.TemporaryDirectory() as temp_dir:
            config = FrameConfig(
                photos_directory="/test/save", slideshow_duration=15, fade_duration=2500
            )

            config_path = Path(temp_dir) / "test-config.json"
            config.save(config_path)

            # Verify file was created and contains correct data
            assert config_path.exists()

            with open(config_path, "r") as f:
                saved_data = json.load(f)

            expected = {
                "photos_directory": "/test/save",
                "slideshow_duration": 15,
                "fade_duration": 2500,
                "import_directory": None,
            }
            assert saved_data == expected

    def test_save_config_creates_directory(self):
        """Test that save creates parent directories if they don't exist"""
        with tempfile.TemporaryDirectory() as temp_dir:
            config = FrameConfig()

            # Save to nested path that doesn't exist
            config_path = Path(temp_dir) / "nested" / "dirs" / "config.json"
            config.save(config_path)

            assert config_path.exists()
            assert config_path.parent.is_dir()

    def test_save_config_default_path(self):
        """Test saving config with default path"""
        with tempfile.TemporaryDirectory() as temp_dir:
            original_cwd = os.getcwd()
            try:
                os.chdir(temp_dir)

                config = FrameConfig(photos_directory="/test/default")
                config.save()  # Use default path

                config_path = Path("frame-config.json")
                assert config_path.exists()

                # Verify content
                with open(config_path, "r") as f:
                    saved_data = json.load(f)

                assert saved_data["photos_directory"] == "/test/default"

            finally:
                os.chdir(original_cwd)

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
        }
        assert config_dict == expected
