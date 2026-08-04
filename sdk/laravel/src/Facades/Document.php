<?php

declare(strict_types=1);

namespace Pliego\Laravel\Facades;

use Illuminate\Support\Facades\Facade;
use Pliego\Laravel\DocumentFactory;

/**
 * @method static \Pliego\Laravel\PendingDocument view(string $name, array $data = [])
 */
final class Document extends Facade
{
    protected static function getFacadeAccessor(): string
    {
        return DocumentFactory::class;
    }
}
