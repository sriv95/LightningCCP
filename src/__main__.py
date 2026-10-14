import logging

from dotenv import load_dotenv

from src.bot import LightningBot
from src.config import load_config


def main() -> None:
    load_dotenv()
    logging.basicConfig(
        level=logging.INFO, format="%(asctime)s %(levelname)s %(name)s: %(message)s"
    )
    config = load_config()
    LightningBot(config).run(config.token, log_handler=None)


if __name__ == "__main__":
    main()
